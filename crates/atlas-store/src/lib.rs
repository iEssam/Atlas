//! SQLite-backed local store (tech-stack.md §4.2).
//!
//! Holds entities, events, and — as of schema v4 (M-TSDB) — Gorilla-compressed
//! numeric samples as opaque `sample_block` BLOBs produced by `atlas-tsdb`. The
//! interim `proc_sample` / `sys_sample` per-window aggregate tables are
//! deprecated (kept for old data, no longer written); the block store keeps
//! every 1 s sample at a fraction of the disk cost, staying within the
//! write-amplification budget (PRD §12.4).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use atlas_tsdb::{EncodedBlock, Metric, SeriesKey};
use rusqlite::{params, Connection, OptionalExtension};

// DEPRECATED as of schema v4 (M-TSDB): `proc_sample` and `sys_sample` are no
// longer written. Numeric samples now live in `sample_block` as Gorilla-
// compressed BLOBs (see SCHEMA_V4). The tables and any existing rows are left
// in place for backward compatibility and are still swept by retention.
const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS process_instance (
    id INTEGER PRIMARY KEY,
    pid INTEGER NOT NULL,
    create_time_100ns INTEGER NOT NULL,
    parent_pid INTEGER NOT NULL,
    session_id INTEGER NOT NULL,
    image_name TEXT NOT NULL,
    first_seen_ms INTEGER NOT NULL,
    last_seen_ms INTEGER NOT NULL,
    exit_seen_ms INTEGER,
    UNIQUE(pid, create_time_100ns)
);

CREATE TABLE IF NOT EXISTS proc_sample (
    ts_ms INTEGER NOT NULL,
    window_secs INTEGER NOT NULL,
    proc_id INTEGER NOT NULL REFERENCES process_instance(id),
    cpu_avg_permille INTEGER NOT NULL,
    cpu_max_permille INTEGER NOT NULL,
    working_set_max INTEGER NOT NULL,
    private_bytes_max INTEGER NOT NULL,
    read_bps_avg INTEGER NOT NULL,
    write_bps_avg INTEGER NOT NULL,
    handles_last INTEGER NOT NULL,
    threads_last INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_proc_sample_ts ON proc_sample(ts_ms);
CREATE INDEX IF NOT EXISTS ix_proc_sample_proc ON proc_sample(proc_id, ts_ms);

CREATE TABLE IF NOT EXISTS sys_sample (
    ts_ms INTEGER PRIMARY KEY,
    cpu_permille INTEGER NOT NULL,
    mem_used INTEGER NOT NULL,
    mem_total INTEGER NOT NULL,
    commit_used INTEGER NOT NULL,
    commit_limit INTEGER NOT NULL,
    process_count INTEGER NOT NULL,
    thread_count INTEGER NOT NULL,
    handle_count INTEGER NOT NULL
);
"#;

// Additive v2 migration: self-metrics (PRD §12.2 — the product must show its
// own overhead) and gap events (PRD §11.3 — degradation is never silent).
// Both tables are created with IF NOT EXISTS so upgrading a v1 database in
// place is a no-op beyond bumping user_version.
const SCHEMA_V2: &str = r#"
CREATE TABLE IF NOT EXISTS self_sample (
    ts_ms INTEGER PRIMARY KEY,
    cpu_permille INTEGER NOT NULL,
    working_set INTEGER NOT NULL,
    tick_duration_us_avg INTEGER NOT NULL,
    tick_duration_us_max INTEGER NOT NULL,
    ticks INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS gap_event (
    ts_ms INTEGER NOT NULL,
    dropped_windows INTEGER NOT NULL,
    reason TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_gap_event_ts ON gap_event(ts_ms);
"#;

// Additive v3 migration (docs/phases.md M3): exact process lifecycle events
// from ETW. `proc_event` is the raw event log (start/stop) with the event's own
// timestamp; `process_instance` gains an `exit_status` column so a Stop event
// can stamp the exact exit code onto the matching live instance.
//
// `proc_event` is created with IF NOT EXISTS; the `exit_status` column is added
// with a guarded ALTER TABLE (SQLite has no `ADD COLUMN IF NOT EXISTS`) so
// upgrading a v2 database in place is a no-op beyond bumping user_version.
const SCHEMA_V3_PROC_EVENT: &str = r#"
CREATE TABLE IF NOT EXISTS proc_event (
    ts_ms INTEGER NOT NULL,
    pid INTEGER NOT NULL,
    kind INTEGER NOT NULL,           -- 0 = start, 1 = stop
    parent_pid INTEGER,              -- start only
    session_id INTEGER,              -- start only
    image_name TEXT,                 -- start only (path/name from the event)
    exit_status INTEGER              -- stop only
);
CREATE INDEX IF NOT EXISTS ix_proc_event_ts ON proc_event(ts_ms);
CREATE INDEX IF NOT EXISTS ix_proc_event_pid ON proc_event(pid, ts_ms);
"#;

// Additive v4 migration (docs/phases.md M-TSDB): Gorilla-compressed sample
// blocks. Each row is one sealed series block — `metric`/`scope` identify the
// series, `start_ms`/`end_ms`/`points` are denormalised header fields the range
// query indexes on (so it never decodes a block to test overlap), and `payload`
// is the opaque encoded block from atlas-tsdb. This replaces per-window
// `proc_sample` / `sys_sample` writes (both now deprecated). Created with
// IF NOT EXISTS so a v3 database upgrades in place.
const SCHEMA_V4: &str = r#"
CREATE TABLE IF NOT EXISTS sample_block (
    metric   INTEGER NOT NULL,
    scope    INTEGER NOT NULL,
    start_ms INTEGER NOT NULL,
    end_ms   INTEGER NOT NULL,
    points   INTEGER NOT NULL,
    payload  BLOB    NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_sample_block_series
    ON sample_block(metric, scope, start_ms);
CREATE INDEX IF NOT EXISTS ix_sample_block_time
    ON sample_block(start_ms);
"#;

// Additive v5 migration (docs/phases.md M6): incident bookmarks, the safe-
// action audit trail, and FTS5 full-text search indexes over process instances
// and bookmarks.
//
// `bookmark` is a plain append-only table (id, ts_ms, label, created_ms).
// `audit` is the broker's append-only decision log — every PrepareAction and
// ExecuteAction lands one row regardless of outcome (PRD §9.22).
//
// Search uses SQLite FTS5 (bundled with rusqlite's `bundled` feature). Two
// contentless-external FTS5 tables mirror the searchable text of
// `process_instance` (image_name + pid as text) and `bookmark` (label); they
// are kept in sync by triggers so inserts/updates/deletes on the base tables
// propagate automatically. If a build ever lacks FTS5, [`Store::migrate`]
// skips these objects and [`Store::search`] falls back to a LIKE scan.
//
// All objects are created IF NOT EXISTS so a v4 database upgrades in place.
const SCHEMA_V5: &str = r#"
CREATE TABLE IF NOT EXISTS bookmark (
    id         INTEGER PRIMARY KEY,
    ts_ms      INTEGER NOT NULL,
    label      TEXT    NOT NULL,
    created_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_bookmark_ts ON bookmark(ts_ms);

CREATE TABLE IF NOT EXISTS audit (
    id        INTEGER PRIMARY KEY,
    ts_ms     INTEGER NOT NULL,
    actor     TEXT    NOT NULL,
    action    TEXT    NOT NULL,
    pid       INTEGER NOT NULL,
    image_name TEXT   NOT NULL,
    decision  TEXT    NOT NULL,
    detail    TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_audit_ts ON audit(ts_ms);
"#;

// FTS5 objects, applied only when the runtime confirms the FTS5 module is
// present. Kept separate from SCHEMA_V5 so a build without FTS5 can still take
// the v5 migration (bookmark + audit) and fall back to LIKE search.
//
// The two indexes are external-content FTS tables (`content=''`, i.e. the FTS
// stores its own copy of the indexed text keyed by the base row's rowid). Sync
// triggers on the base tables mirror insert/update/delete. `pid` is indexed as
// text so `search 4242` matches by pid.
const SCHEMA_V5_FTS: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS process_fts USING fts5(
    image_name,
    pid,
    content=''
);
CREATE VIRTUAL TABLE IF NOT EXISTS bookmark_fts USING fts5(
    label,
    content=''
);

CREATE TRIGGER IF NOT EXISTS process_fts_ai AFTER INSERT ON process_instance BEGIN
    INSERT INTO process_fts(rowid, image_name, pid)
    VALUES (new.id, new.image_name, CAST(new.pid AS TEXT));
END;
CREATE TRIGGER IF NOT EXISTS process_fts_ad AFTER DELETE ON process_instance BEGIN
    INSERT INTO process_fts(process_fts, rowid, image_name, pid)
    VALUES ('delete', old.id, old.image_name, CAST(old.pid AS TEXT));
END;
CREATE TRIGGER IF NOT EXISTS process_fts_au AFTER UPDATE ON process_instance BEGIN
    INSERT INTO process_fts(process_fts, rowid, image_name, pid)
    VALUES ('delete', old.id, old.image_name, CAST(old.pid AS TEXT));
    INSERT INTO process_fts(rowid, image_name, pid)
    VALUES (new.id, new.image_name, CAST(new.pid AS TEXT));
END;

CREATE TRIGGER IF NOT EXISTS bookmark_fts_ai AFTER INSERT ON bookmark BEGIN
    INSERT INTO bookmark_fts(rowid, label) VALUES (new.id, new.label);
END;
CREATE TRIGGER IF NOT EXISTS bookmark_fts_ad AFTER DELETE ON bookmark BEGIN
    INSERT INTO bookmark_fts(bookmark_fts, rowid, label) VALUES ('delete', old.id, old.label);
END;
"#;

// Additive v6 migration (docs/phases.md M7): privacy-capability usage history.
// `privacy_event` is the append-only log of camera/mic/location start/stop
// transitions the ConsentStore watcher records (PRD §9.10). Startup and services
// inventories are enumerated LIVE from the OS on each request (they are current-
// state, not history) and therefore get no tables here — only privacy has a
// meaningful event timeline. Created with IF NOT EXISTS so a v5 database upgrades
// in place.
const SCHEMA_V6: &str = r#"
CREATE TABLE IF NOT EXISTS privacy_event (
    ts_ms        INTEGER NOT NULL,
    capability   INTEGER NOT NULL,   -- 1=camera, 2=microphone, 3=location
    app_id       TEXT    NOT NULL,
    display_name TEXT    NOT NULL,
    started      INTEGER NOT NULL    -- 1=start, 0=stop
);
CREATE INDEX IF NOT EXISTS ix_privacy_event_ts ON privacy_event(ts_ms);
"#;

// Additive v7 migration (docs/phases.md M8): detected incidents. Each row is one
// threshold+duration incident the detectors found over the recorded series
// (PRD §9.3.7): `kind`/`severity` carry the proto `IncidentKind`/`Severity`
// discriminants, `start_ms`/`end_ms` bound the episode (`end_ms` NULL = still
// ongoing at last observation), `peak_value` is the peak of the driving metric
// (CPU percent or memory percent), and `summary` is the plain-language one-liner.
//
// `UNIQUE(kind, start_ms)` makes detection idempotent: re-running a detection
// pass over an overlapping window upserts the same episode (extending its end /
// peak) rather than spawning duplicates, since a run's start timestamp is stable.
// Created with IF NOT EXISTS so a v6 database upgrades in place.
const SCHEMA_V7: &str = r#"
CREATE TABLE IF NOT EXISTS incident (
    id         INTEGER PRIMARY KEY,
    kind       INTEGER NOT NULL,   -- proto IncidentKind: 1=cpu, 2=memory, 3=disk
    start_ms   INTEGER NOT NULL,
    end_ms     INTEGER,            -- NULL = ongoing at last observation
    severity   INTEGER NOT NULL,   -- proto Severity: 1=info, 2=warning, 3=critical
    peak_value REAL    NOT NULL,
    summary    TEXT    NOT NULL,
    UNIQUE(kind, start_ms)
);
CREATE INDEX IF NOT EXISTS ix_incident_start ON incident(start_ms);
"#;

// Additive v8 migration (docs/phases.md Phase 2 / R2): the performance rules
// engine (PRD §9.7). `rule` is the persisted rule store — each row is a data
// document (never code): an image-name match + a trigger + the reversible action
// set (priority class / core affinity / EcoQoS) + a precedence for conflict
// resolution. `profile` is a named, activatable bundle with an optional power
// mode; `profile_rule` links profiles to the rules they toggle. All three
// persist across restarts so rules/profiles survive a service bounce. The action
// enums (trigger/priority_class/affinity_mode) carry the proto discriminants so
// the store stays wire-shaped. Created IF NOT EXISTS so a v7 database upgrades in
// place. `affinity_mask` is a u64 processor bitmask stored bit-for-bit as i64.
const SCHEMA_V8: &str = r#"
CREATE TABLE IF NOT EXISTS rule (
    id             INTEGER PRIMARY KEY,
    name           TEXT    NOT NULL,
    enabled        INTEGER NOT NULL,
    match_image    TEXT    NOT NULL,
    trigger        INTEGER NOT NULL,   -- proto RuleTrigger discriminant
    priority_class INTEGER NOT NULL,   -- proto PriorityClass discriminant
    affinity_mode  INTEGER NOT NULL,   -- proto CoreAffinityMode discriminant
    affinity_mask  INTEGER NOT NULL,   -- u64 processor bitmask (bit-cast to i64)
    eco_qos        INTEGER NOT NULL,   -- 1 = enable EcoQoS
    precedence     INTEGER NOT NULL,   -- higher wins on conflict
    created_ms     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_rule_enabled ON rule(enabled);

CREATE TABLE IF NOT EXISTS profile (
    id         INTEGER PRIMARY KEY,
    name       TEXT    NOT NULL,
    power_mode TEXT    NOT NULL,   -- "" | PowerSaver | Balanced | HighPerformance
    active     INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS profile_rule (
    profile_id INTEGER NOT NULL REFERENCES profile(id) ON DELETE CASCADE,
    rule_id    INTEGER NOT NULL REFERENCES rule(id)    ON DELETE CASCADE,
    UNIQUE(profile_id, rule_id)
);
CREATE INDEX IF NOT EXISTS ix_profile_rule_profile ON profile_rule(profile_id);
"#;

// Additive v9 migration (docs/phases.md R2 / PRD §9.10.3): advanced privacy
// alerts. `privacy_alert_rule` is the persisted alert-rule store — each row is a
// data document: which capability to watch (0 = all), the condition discriminant
// (proto `PrivacyAlertCondition`), and a duration threshold for ALERT_LONGER_THAN.
// `fired_alert` is the append-only log the ConsentStore change-watcher's evaluator
// writes when a rule matches a transition; `detail` is a FACTUAL, never accusatory
// one-liner. Rules persist across restarts; fired alerts are history the
// `ListFiredAlerts` RPC reads back. Created IF NOT EXISTS so a v8 database
// upgrades in place. The privacy_event table (v6) is finally populated by the
// same watcher, so its usage history stops being empty.
const SCHEMA_V9: &str = r#"
CREATE TABLE IF NOT EXISTS privacy_alert_rule (
    id                INTEGER PRIMARY KEY,
    name              TEXT    NOT NULL,
    enabled           INTEGER NOT NULL,
    capability        INTEGER NOT NULL,   -- 0=all, 1=camera, 2=microphone, 3=location
    condition         INTEGER NOT NULL,   -- proto PrivacyAlertCondition discriminant
    threshold_seconds INTEGER NOT NULL,   -- for ALERT_LONGER_THAN
    created_ms        INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_privacy_alert_rule_enabled ON privacy_alert_rule(enabled);

CREATE TABLE IF NOT EXISTS fired_alert (
    id           INTEGER PRIMARY KEY,
    rule_id      INTEGER NOT NULL,
    ts_ms        INTEGER NOT NULL,
    capability   INTEGER NOT NULL,   -- 1=camera, 2=microphone, 3=location
    app_id       TEXT    NOT NULL,
    display_name TEXT    NOT NULL,
    detail       TEXT    NOT NULL    -- factual, never accusatory
);
CREATE INDEX IF NOT EXISTS ix_fired_alert_ts ON fired_alert(ts_ms);
"#;

// Additive v10 migration (docs/phases.md Phase 3 / R3, PRD §9.13/§9.14):
// forensics — system-change tracking + crash correlation.
//
// `system_change` is the append-only log the periodic change-detector writes: each
// row is one detected change (`kind` carries the proto `SystemChangeKind`
// discriminant), `detail` a human before→after summary, `reversible` a
// this-milestone-informational flag. Rows are diff-sourced (inventory diffing) or
// event-sourced (WUA update history); the table doesn't distinguish — the `kind`
// does. No natural unique key: a change is a point-in-time event, and the
// inventory-snapshot baseline (below) makes the detector idempotent by only
// emitting rows when the inventory actually moved.
//
// `inventory_snapshot` is the detector's "last inventory" baseline — one row per
// snapshot key (the detector stores the whole inventory as one JSON blob under
// key 'full'; per-kind keys are also allowed). It is opaque TEXT to the store;
// only the service (which owns serde) parses it.
//
// `crash_record` is the correlated crash log the crash-scanner writes: `kind`
// carries the proto `CrashKind` discriminant, `context` is a JSON array of the
// factual, hedged correlation strings the service assembles (peak memory/CPU,
// recent changes, repeated-restart note). `UNIQUE(ts_ms, kind, subject)` makes a
// re-scan of the same log window idempotent — a repeated scan refreshes `fault`/
// `exception_code`/`context` on the existing row rather than duplicating it.
//
// All objects are created IF NOT EXISTS so a v9 database upgrades in place.
const SCHEMA_V10: &str = r#"
CREATE TABLE IF NOT EXISTS system_change (
    id          INTEGER PRIMARY KEY,
    ts_ms       INTEGER NOT NULL,
    kind        INTEGER NOT NULL,   -- proto SystemChangeKind discriminant
    subject     TEXT    NOT NULL,
    detail      TEXT    NOT NULL,   -- human-readable before→after summary
    publisher   TEXT    NOT NULL,
    responsible TEXT    NOT NULL,
    reversible  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_system_change_ts ON system_change(ts_ms);

CREATE TABLE IF NOT EXISTS inventory_snapshot (
    kind       TEXT    PRIMARY KEY,   -- 'full' (or a per-inventory-kind key)
    json       TEXT    NOT NULL,
    updated_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS crash_record (
    id             INTEGER PRIMARY KEY,
    ts_ms          INTEGER NOT NULL,
    kind           INTEGER NOT NULL,   -- proto CrashKind discriminant
    subject        TEXT    NOT NULL,
    fault          TEXT    NOT NULL,
    exception_code TEXT    NOT NULL,
    context        TEXT    NOT NULL,   -- JSON array of factual, hedged strings
    UNIQUE(ts_ms, kind, subject)
);
CREATE INDEX IF NOT EXISTS ix_crash_record_ts ON crash_record(ts_ms);
"#;

/// Whether `table` already has a column named `column`, via `PRAGMA table_info`.
/// Used to make the v3 `ADD COLUMN` migration idempotent (SQLite lacks
/// `ADD COLUMN IF NOT EXISTS`).
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Debug, Clone)]
pub struct ProcIdentity {
    pub pid: u32,
    pub create_time_100ns: i64,
    pub parent_pid: u32,
    pub session_id: u32,
    pub image_name: String,
}

/// One row per flush window recording Atlas's own overhead (PRD §12.2).
#[derive(Debug, Clone, Copy)]
pub struct SelfSampleRow {
    pub ts_ms: i64,
    /// Own-process CPU over the window, in permille (0..=1000).
    pub cpu_permille: u32,
    /// Own-process working set at flush, in bytes.
    pub working_set: u64,
    /// Mean `sampler.sample()` duration over the window, microseconds.
    pub tick_duration_us_avg: u64,
    /// Worst `sampler.sample()` duration over the window, microseconds.
    pub tick_duration_us_max: u64,
    /// Ticks folded into this window.
    pub ticks: u32,
}

/// A raw process lifecycle event, as delivered by ETW (docs/phases.md M3).
/// `kind` is 0 for start, 1 for stop; the optional fields carry only what the
/// matching event kind supplies (start: parent/session/image; stop: exit).
#[derive(Debug, Clone)]
pub struct ProcEventRow {
    pub ts_ms: i64,
    pub pid: u32,
    pub kind: u8,
    pub parent_pid: Option<u32>,
    pub session_id: Option<u32>,
    pub image_name: Option<String>,
    pub exit_status: Option<i32>,
}

/// `proc_event.kind` discriminants.
pub const PROC_EVENT_START: u8 = 0;
pub const PROC_EVENT_STOP: u8 = 1;

/// One recorded privacy-capability usage transition (docs/phases.md M7,
/// PRD §9.10). `capability` is 1=camera, 2=microphone, 3=location (matching the
/// proto `CapabilityKind`); `started` is true for a start, false for a stop.
#[derive(Debug, Clone)]
pub struct PrivacyEventRow {
    pub ts_ms: i64,
    pub capability: i32,
    pub app_id: String,
    pub display_name: String,
    pub started: bool,
}

/// One stored sample block returned from a range query, with its decoded
/// points. The store validates and decodes the payload via `atlas-tsdb`, so
/// callers get `(ts_ms, value)` pairs directly and never touch the byte format.
#[derive(Debug, Clone)]
pub struct DecodedBlock {
    pub key: SeriesKey,
    pub start_ms: i64,
    pub end_ms: i64,
    pub points: Vec<(i64, f64)>,
}

/// One decimation bucket returned by [`Store::query_range`] (M6). Carries the
/// min/max (spike-preserving), the mean, and the sample count folded into the
/// bucket. Empty buckets are never emitted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RangeBucketRow {
    pub start_ms: i64,
    pub min: f64,
    pub max: f64,
    pub avg: f64,
    pub samples: u32,
}

/// One process lifecycle event as returned by [`Store::list_events`] — the
/// stored `proc_event` row shaped for the wire (M6). `has_exit_status` mirrors
/// the proto's explicit presence flag so a stop with a genuine 0 exit code is
/// distinguishable from an unknown one.
#[derive(Debug, Clone)]
pub struct EventListRow {
    pub ts_ms: i64,
    pub kind: u32,
    pub pid: u32,
    pub parent_pid: u32,
    pub session_id: u32,
    pub image_name: String,
    pub exit_status: i32,
    pub has_exit_status: bool,
}

/// One process-instance search hit (M6): a `process_instance` row matched by
/// name or pid, with its identity and liveness. `exit_seen_ms` is 0 while live.
#[derive(Debug, Clone)]
pub struct ProcessHitRow {
    pub proc_row_id: i64,
    pub pid: u32,
    pub image_name: String,
    pub first_seen_ms: i64,
    pub exit_seen_ms: i64,
    pub live: bool,
}

/// One stored incident bookmark (PRD §9.3.6).
#[derive(Debug, Clone)]
pub struct BookmarkRow {
    pub id: i64,
    pub ts_ms: i64,
    pub label: String,
    pub created_ms: i64,
}

/// The typed result set of a [`Store::search`] call: process instances, events,
/// and bookmarks that matched the query. The service maps each into a proto
/// `SearchHit` oneof arm.
#[derive(Debug, Clone, Default)]
pub struct SearchHits {
    pub processes: Vec<ProcessHitRow>,
    pub events: Vec<EventListRow>,
    pub bookmarks: Vec<BookmarkRow>,
}

/// One safe-action audit row (PRD §9.22, docs/phases.md M6). Every broker
/// prepare and execute appends one of these regardless of outcome. Text-valued
/// so the log is human-readable straight out of SQLite: `action` is the action
/// name ("SUSPEND", ...), `decision` is the verdict phase+result
/// ("PREPARE_ALLOWED" / "PREPARE_DENIED" / "EXECUTE_OK" / "EXECUTE_FAIL"), and
/// `detail` is the free-form reason or result message.
#[derive(Debug, Clone)]
pub struct AuditRow {
    pub ts_ms: i64,
    /// Who requested it. Always "local-ui" for the broker v0 (the pipe DACL is
    /// the actual principal boundary; there is one local actor).
    pub actor: String,
    /// Action name, e.g. "CLOSE_WINDOWS" / "SUSPEND" / "RESUME" / "TERMINATE".
    pub action: String,
    pub pid: u32,
    pub image_name: String,
    /// Verdict/phase tag, e.g. "PREPARE_ALLOWED" / "PREPARE_DENIED" /
    /// "EXECUTE_OK" / "EXECUTE_FAIL".
    pub decision: String,
    /// Free-form reason (denial cause, result message, error text).
    pub detail: String,
}

/// One detected incident (docs/phases.md M8, PRD §9.3.7). `kind` and `severity`
/// carry the proto `IncidentKind`/`Severity` discriminants; `end_ms` is `None`
/// while the incident is still ongoing at the last observation. `peak_value` is
/// the peak of the driving metric over the window — CPU percent or memory
/// percent (0..=100).
#[derive(Debug, Clone, PartialEq)]
pub struct IncidentRow {
    pub id: i64,
    pub kind: i32,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    pub severity: i32,
    pub peak_value: f64,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct TopProcessRow {
    pub pid: u32,
    pub image_name: String,
    pub cpu_avg_permille: f64,
    pub cpu_peak_permille: u32,
    pub working_set_peak: u64,
    pub windows: u32,
}

/// One persisted performance rule (docs/phases.md R2, PRD §9.7). Field-for-field
/// with the proto `Rule` + flattened `RuleAction`: the enum fields carry the
/// proto discriminants (`RuleTrigger` / `PriorityClass` / `CoreAffinityMode`) so
/// the store row maps straight onto the wire type. `affinity_mask` is the u64
/// processor bitmask (only meaningful when `affinity_mode` = CUSTOM_MASK).
#[derive(Debug, Clone, PartialEq)]
pub struct RuleRow {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub match_image: String,
    pub trigger: i32,
    pub priority_class: i32,
    pub affinity_mode: i32,
    pub affinity_mask: u64,
    pub eco_qos: bool,
    pub precedence: i32,
    pub created_ms: i64,
}

/// One persisted profile: a named, activatable bundle of rule ids plus an
/// optional power mode (PRD §9.7.4). `rule_ids` is the set of rules the profile
/// toggles; `active` records whether it is currently applied.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileRow {
    pub id: i64,
    pub name: String,
    pub power_mode: String,
    pub active: bool,
    pub rule_ids: Vec<i64>,
}

/// One persisted advanced-privacy-alert rule (schema v9, PRD §9.10.3). Mirrors
/// the proto `PrivacyAlertRule`: `capability` is the proto `CapabilityKind`
/// discriminant (0 = all), `condition` the `PrivacyAlertCondition` discriminant,
/// and `threshold_seconds` applies only to ALERT_LONGER_THAN.
#[derive(Debug, Clone, PartialEq)]
pub struct PrivacyAlertRuleRow {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub capability: i32,
    pub condition: i32,
    pub threshold_seconds: u32,
    pub created_ms: i64,
}

/// One recorded fired privacy alert (schema v9). `capability` is the proto
/// `CapabilityKind` discriminant; `detail` is a factual, never-accusatory string.
/// `rule_name` is resolved by join at read time (empty when the rule was deleted).
#[derive(Debug, Clone, PartialEq)]
pub struct FiredAlertRow {
    pub id: i64,
    pub rule_id: i64,
    pub rule_name: String,
    pub ts_ms: i64,
    pub capability: i32,
    pub app_id: String,
    pub display_name: String,
    pub detail: String,
}

/// One recorded system change (schema v10, PRD §9.13). Mirrors the proto
/// `SystemChange`: `kind` is the proto `SystemChangeKind` discriminant, `detail` a
/// human before→after summary. `id` is assigned by the store on insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemChangeRow {
    pub id: i64,
    pub ts_ms: i64,
    pub kind: i32,
    pub subject: String,
    pub detail: String,
    pub publisher: String,
    pub responsible: String,
    pub reversible: bool,
}

/// One recorded, correlated crash (schema v10, PRD §9.14). Mirrors the proto
/// `CrashRecord`: `kind` is the proto `CrashKind` discriminant; `context` is the
/// factual, hedged correlation string list the service assembled (stored as a JSON
/// array). `id` is assigned by the store on insert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashRow {
    pub id: i64,
    pub ts_ms: i64,
    pub kind: i32,
    pub subject: String,
    pub fault: String,
    pub exception_code: String,
    pub context: Vec<String>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating data dir {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        // WAL keeps readers unblocked during batch flushes; NORMAL sync is
        // the accepted durability/write-cost point for telemetry data.
        conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get::<_, String>(0))?;
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        // Enforce the profile_rule → rule/profile foreign keys so deleting a rule
        // or profile cascades its link rows (R2 rules engine, schema v8).
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            self.conn.execute_batch(SCHEMA_V1)?;
            self.conn.execute_batch("PRAGMA user_version = 1;")?;
        }
        if version < 2 {
            self.conn.execute_batch(SCHEMA_V2)?;
            self.conn.execute_batch("PRAGMA user_version = 2;")?;
        }
        if version < 3 {
            self.conn.execute_batch(SCHEMA_V3_PROC_EVENT)?;
            // Add exit_status to process_instance if it isn't already present.
            // SQLite lacks `ADD COLUMN IF NOT EXISTS`, so probe the table info
            // first; a v1/v2 database won't have the column, a re-run would.
            if !column_exists(&self.conn, "process_instance", "exit_status")? {
                self.conn.execute_batch(
                    "ALTER TABLE process_instance ADD COLUMN exit_status INTEGER;",
                )?;
            }
            self.conn.execute_batch("PRAGMA user_version = 3;")?;
        }
        if version < 4 {
            self.conn.execute_batch(SCHEMA_V4)?;
            self.conn.execute_batch("PRAGMA user_version = 4;")?;
        }
        if version < 5 {
            self.conn.execute_batch(SCHEMA_V5)?;
            self.conn.execute_batch("PRAGMA user_version = 5;")?;
        }
        if version < 6 {
            self.conn.execute_batch(SCHEMA_V6)?;
            self.conn.execute_batch("PRAGMA user_version = 6;")?;
        }
        if version < 7 {
            self.conn.execute_batch(SCHEMA_V7)?;
            self.conn.execute_batch("PRAGMA user_version = 7;")?;
        }
        if version < 8 {
            self.conn.execute_batch(SCHEMA_V8)?;
            self.conn.execute_batch("PRAGMA user_version = 8;")?;
        }
        if version < 9 {
            self.conn.execute_batch(SCHEMA_V9)?;
            self.conn.execute_batch("PRAGMA user_version = 9;")?;
        }
        if version < 10 {
            self.conn.execute_batch(SCHEMA_V10)?;
            self.conn.execute_batch("PRAGMA user_version = 10;")?;
        }
        // FTS5 objects are built (idempotently, IF NOT EXISTS) on every open
        // once the module is confirmed present, independent of user_version: a
        // v5 database created on a no-FTS5 build must gain the indexes the first
        // time it is opened on an FTS5-capable build. When FTS5 is absent the
        // search path falls back to LIKE (see `search`).
        if self.has_fts5() {
            self.conn.execute_batch(SCHEMA_V5_FTS)?;
            self.backfill_fts()?;
        }
        Ok(())
    }

    /// Populates the FTS indexes from existing base-table rows the first time
    /// the indexes appear (a database recorded before FTS5 was available, or
    /// before the v5 migration's triggers existed). Idempotent: it only inserts
    /// rows whose rowid is not already indexed, so repeated opens are cheap.
    fn backfill_fts(&self) -> Result<()> {
        self.conn.execute_batch(
            "INSERT INTO process_fts(rowid, image_name, pid)
                 SELECT id, image_name, CAST(pid AS TEXT) FROM process_instance pi
                 WHERE NOT EXISTS (SELECT 1 FROM process_fts f WHERE f.rowid = pi.id);
             INSERT INTO bookmark_fts(rowid, label)
                 SELECT id, label FROM bookmark b
                 WHERE NOT EXISTS (SELECT 1 FROM bookmark_fts f WHERE f.rowid = b.id);",
        )?;
        Ok(())
    }

    /// Whether the bundled SQLite has FTS5 compiled in (PRAGMA compile_options).
    /// The M6 search path prefers FTS5 when present and falls back to LIKE with
    /// a capability note otherwise (docs/phases.md M6).
    pub fn has_fts5(&self) -> bool {
        self.conn
            .prepare("PRAGMA compile_options")
            .and_then(|mut stmt| {
                let mut rows = stmt.query([])?;
                let mut found = false;
                while let Some(row) = rows.next()? {
                    let opt: String = row.get(0)?;
                    if opt.eq_ignore_ascii_case("ENABLE_FTS5") {
                        found = true;
                        break;
                    }
                }
                Ok(found)
            })
            .unwrap_or(false)
    }

    /// Returns the stable row id for a process instance, inserting it on
    /// first sight and refreshing `last_seen_ms` otherwise.
    pub fn upsert_process(&self, p: &ProcIdentity, now_ms: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO process_instance
                 (pid, create_time_100ns, parent_pid, session_id, image_name,
                  first_seen_ms, last_seen_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
             ON CONFLICT(pid, create_time_100ns)
             DO UPDATE SET last_seen_ms = excluded.last_seen_ms",
            params![
                p.pid,
                p.create_time_100ns,
                p.parent_pid,
                p.session_id,
                p.image_name,
                now_ms
            ],
        )?;
        let id = self.conn.query_row(
            "SELECT id FROM process_instance WHERE pid = ?1 AND create_time_100ns = ?2",
            params![p.pid, p.create_time_100ns],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    pub fn mark_exited(&self, proc_row_id: i64, now_ms: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE process_instance SET exit_seen_ms = ?2 WHERE id = ?1",
            params![proc_row_id, now_ms],
        )?;
        Ok(())
    }

    /// One transaction per flush window — the only place samples touch disk.
    /// As of schema v4 (M-TSDB) numeric samples ride as Gorilla `blocks` rather
    /// than `proc_sample`/`sys_sample` rows (both deprecated, no longer written).
    /// `proc_events` are the raw ETW start/stop rows drained during the window;
    /// they ride the same transaction so an event and its window land together.
    pub fn write_batch(
        &mut self,
        blocks: &[EncodedBlock],
        proc_events: &[ProcEventRow],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            if !blocks.is_empty() {
                let mut blk_stmt = tx.prepare_cached(
                    "INSERT INTO sample_block
                         (metric, scope, start_ms, end_ms, points, payload)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )?;
                for b in blocks {
                    blk_stmt.execute(params![
                        b.key.metric.as_u16() as i64,
                        b.key.scope,
                        b.start_ms,
                        b.end_ms,
                        b.points as i64,
                        b.payload
                    ])?;
                }
            }

            if !proc_events.is_empty() {
                let mut ev_stmt = tx.prepare_cached(
                    "INSERT INTO proc_event
                         (ts_ms, pid, kind, parent_pid, session_id, image_name, exit_status)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?;
                for e in proc_events {
                    ev_stmt.execute(params![
                        e.ts_ms,
                        e.pid,
                        e.kind,
                        e.parent_pid,
                        e.session_id,
                        e.image_name,
                        e.exit_status
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Persists sealed sample blocks in their own transaction. Used where there
    /// are no accompanying events to batch (e.g. a scope-drain on process exit).
    pub fn write_blocks(&mut self, blocks: &[EncodedBlock]) -> Result<()> {
        self.write_batch(blocks, &[])
    }

    /// Reads sample blocks for `metric` overlapping the `[from_ms, to_ms]`
    /// window, decoding each payload to points via atlas-tsdb. `scope_filter`
    /// restricts to one series scope when `Some` (a process row id, or 0 for
    /// system); `None` returns every scope for the metric.
    ///
    /// Overlap uses the denormalised `start_ms`/`end_ms` header columns so the
    /// index does the pruning; a corrupt block surfaces as an error (never a
    /// panic) and aborts the read — corruption is not silently skipped.
    pub fn read_blocks(
        &self,
        metric: Metric,
        scope_filter: Option<i64>,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<DecodedBlock>> {
        let metric_id = metric.as_u16() as i64;
        // A block overlaps the window iff start_ms <= to AND end_ms >= from.
        let mut out = Vec::new();
        let mut push = |scope: i64, start_ms: i64, end_ms: i64, payload: Vec<u8>| -> Result<()> {
            let reader = atlas_tsdb::BlockReader::parse(&payload)
                .map_err(|e| anyhow::anyhow!("decoding sample_block (scope {scope}): {e}"))?;
            let points = reader.points().map_err(|e| {
                anyhow::anyhow!("decoding sample_block points (scope {scope}): {e}")
            })?;
            out.push(DecodedBlock {
                key: SeriesKey::new(metric, scope),
                start_ms,
                end_ms,
                points,
            });
            Ok(())
        };

        match scope_filter {
            Some(scope) => {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT scope, start_ms, end_ms, payload FROM sample_block
                     WHERE metric = ?1 AND scope = ?2 AND start_ms <= ?4 AND end_ms >= ?3
                     ORDER BY start_ms",
                )?;
                let rows = stmt.query_map(params![metric_id, scope, from_ms, to_ms], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                    ))
                })?;
                for row in rows {
                    let (scope, start_ms, end_ms, payload) = row?;
                    push(scope, start_ms, end_ms, payload)?;
                }
            }
            None => {
                let mut stmt = self.conn.prepare_cached(
                    "SELECT scope, start_ms, end_ms, payload FROM sample_block
                     WHERE metric = ?1 AND start_ms <= ?3 AND end_ms >= ?2
                     ORDER BY scope, start_ms",
                )?;
                let rows = stmt.query_map(params![metric_id, from_ms, to_ms], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                    ))
                })?;
                for row in rows {
                    let (scope, start_ms, end_ms, payload) = row?;
                    push(scope, start_ms, end_ms, payload)?;
                }
            }
        }
        Ok(out)
    }

    /// Total bytes of encoded sample-block payloads on record (SUM of
    /// LENGTH(payload)). Surfaces storage footprint without a SQLite client.
    pub fn sample_storage_bytes(&self) -> Result<u64> {
        let bytes: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(payload)), 0) FROM sample_block",
            [],
            |r| r.get(0),
        )?;
        Ok(bytes as u64)
    }

    /// Total number of samples across all stored blocks (SUM of `points`).
    /// Paired with [`Store::sample_storage_bytes`] to report bytes/sample.
    pub fn sample_count(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(points), 0) FROM sample_block",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Stamps the exact exit timestamp and status onto the currently-live
    /// process instance(s) matching `pid` (docs/phases.md M3 exit stamping).
    ///
    /// Matching is by `pid` against instances that have not yet been marked
    /// exited (`exit_seen_ms IS NULL`). We deliberately do *not* reconstruct the
    /// `(pid, create_time_100ns)` key from the ETW event: the event timestamp is
    /// the process's *stop* time, which never equals the snapshot's CreateTime,
    /// and ETW does not carry CreateTime on the Stop record. Within a single
    /// recording session a live, un-exited instance for a pid is unique in
    /// practice (a pid is not reused until its prior holder has exited, at which
    /// point that instance is already stamped), so the pid match is safe.
    ///
    /// Returns the number of instance rows stamped (0 for a stop-without-start).
    pub fn stamp_exit_by_pid(
        &self,
        pid: u32,
        exit_ms: i64,
        exit_status: Option<i32>,
    ) -> Result<usize> {
        let n = self.conn.execute(
            "UPDATE process_instance
                SET exit_seen_ms = ?2, exit_status = ?3
              WHERE pid = ?1 AND exit_seen_ms IS NULL",
            params![pid, exit_ms, exit_status],
        )?;
        Ok(n)
    }

    /// Deletes samples older than the cutoff. Returns (proc rows, sys rows)
    /// removed from the deprecated interim tables — retained so an in-place
    /// upgraded database still sheds its old `proc_sample`/`sys_sample` rows.
    /// Sample blocks are swept by [`Store::apply_block_retention`].
    pub fn apply_retention(&self, cutoff_ms: i64) -> Result<(usize, usize)> {
        let p = self.conn.execute(
            "DELETE FROM proc_sample WHERE ts_ms < ?1",
            params![cutoff_ms],
        )?;
        let s = self.conn.execute(
            "DELETE FROM sys_sample WHERE ts_ms < ?1",
            params![cutoff_ms],
        )?;
        Ok((p, s))
    }

    /// Deletes sample blocks that end before the cutoff (M-TSDB retention). A
    /// block is dropped only once its whole span is past retention, so a block
    /// straddling the cutoff is kept until it ages out entirely. Returns rows
    /// removed.
    pub fn apply_block_retention(&self, cutoff_ms: i64) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM sample_block WHERE end_ms < ?1",
            params![cutoff_ms],
        )?;
        Ok(n)
    }

    /// Records Atlas's own overhead for one flush window (PRD §12.2).
    pub fn write_self_sample(&self, s: &SelfSampleRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO self_sample
                 (ts_ms, cpu_permille, working_set, tick_duration_us_avg,
                  tick_duration_us_max, ticks)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                s.ts_ms,
                s.cpu_permille,
                s.working_set as i64,
                s.tick_duration_us_avg as i64,
                s.tick_duration_us_max as i64,
                s.ticks
            ],
        )?;
        Ok(())
    }

    /// Records that `dropped_windows` flush windows were discarded because the
    /// writer stalled. Degradation must be observable, never silent (PRD §11.3).
    pub fn record_gap(&self, ts_ms: i64, dropped_windows: u64, reason: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO gap_event (ts_ms, dropped_windows, reason) VALUES (?1, ?2, ?3)",
            params![ts_ms, dropped_windows as i64, reason],
        )?;
        Ok(())
    }

    /// Returns the most recently recorded self-metrics row, if any.
    pub fn latest_self_sample(&self) -> Result<Option<SelfSampleRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT ts_ms, cpu_permille, working_set, tick_duration_us_avg,
                        tick_duration_us_max, ticks
                 FROM self_sample ORDER BY ts_ms DESC LIMIT 1",
                [],
                |r| {
                    Ok(SelfSampleRow {
                        ts_ms: r.get(0)?,
                        cpu_permille: r.get(1)?,
                        working_set: r.get::<_, i64>(2)? as u64,
                        tick_duration_us_avg: r.get::<_, i64>(3)? as u64,
                        tick_duration_us_max: r.get::<_, i64>(4)? as u64,
                        ticks: r.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Top processes by time-weighted average CPU over `[since_ms, now_ms]`,
    /// computed over the Gorilla `sample_block` store (M-TSDB). CPU average is
    /// weighted by each sample's wall-clock gap to its successor (so a variable
    /// cadence doesn't over-count sparse ticks); CPU peak is the max sample in
    /// the window; working-set peak comes from the WorkingSet series. Scopes are
    /// joined to `process_instance` for pid/name. `windows` reports the CPU
    /// sample count contributing (the analogue of the old per-window count).
    pub fn top_processes(
        &self,
        since_ms: i64,
        now_ms: i64,
        limit: u32,
    ) -> Result<Vec<TopProcessRow>> {
        use std::collections::HashMap;

        // Per-scope CPU aggregation from decoded CpuPermille blocks.
        struct CpuAgg {
            weighted_sum: f64,
            weight_s: f64,
            peak: u32,
            samples: u32,
        }
        let mut cpu: HashMap<i64, CpuAgg> = HashMap::new();
        for blk in self.read_blocks(Metric::CpuPermille, None, since_ms, now_ms)? {
            let scope = blk.key.scope;
            let entry = cpu.entry(scope).or_insert(CpuAgg {
                weighted_sum: 0.0,
                weight_s: 0.0,
                peak: 0,
                samples: 0,
            });
            let pts = &blk.points;
            for (i, &(ts, v)) in pts.iter().enumerate() {
                if ts < since_ms || ts > now_ms {
                    continue;
                }
                // Weight by the gap to the next in-window sample; the last
                // sample gets a nominal 1 s so a single point still counts.
                let dt = pts
                    .get(i + 1)
                    .map(|&(nts, _)| (nts - ts).max(0) as f64 / 1000.0)
                    .filter(|d| *d > 0.0)
                    .unwrap_or(1.0);
                entry.weighted_sum += v * dt;
                entry.weight_s += dt;
                entry.peak = entry.peak.max(v.round().max(0.0) as u32);
                entry.samples += 1;
            }
        }

        // Per-scope working-set peak from WorkingSet blocks.
        let mut ws_peak: HashMap<i64, u64> = HashMap::new();
        for blk in self.read_blocks(Metric::WorkingSet, None, since_ms, now_ms)? {
            let peak = ws_peak.entry(blk.key.scope).or_insert(0);
            for &(ts, v) in &blk.points {
                if ts < since_ms || ts > now_ms {
                    continue;
                }
                *peak = (*peak).max(v.max(0.0) as u64);
            }
        }

        // Resolve scope (proc_row_id) → (pid, name) for scopes we saw.
        let mut rows: Vec<TopProcessRow> = Vec::with_capacity(cpu.len());
        let mut name_stmt = self
            .conn
            .prepare_cached("SELECT pid, image_name FROM process_instance WHERE id = ?1")?;
        for (scope, agg) in cpu {
            let avg = if agg.weight_s > 0.0 {
                agg.weighted_sum / agg.weight_s
            } else {
                0.0
            };
            let named = name_stmt
                .query_row(params![scope], |r| {
                    Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?))
                })
                .optional()?;
            let (pid, image_name) = named.unwrap_or((0, format!("scope#{scope}")));
            rows.push(TopProcessRow {
                pid,
                image_name,
                cpu_avg_permille: avg,
                cpu_peak_permille: agg.peak,
                working_set_peak: ws_peak.get(&scope).copied().unwrap_or(0),
                windows: agg.samples,
            });
        }
        rows.sort_by(|a, b| {
            b.cpu_avg_permille
                .partial_cmp(&a.cpu_avg_permille)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.working_set_peak.cmp(&a.working_set_peak))
        });
        rows.truncate(limit as usize);
        Ok(rows)
    }

    /// Lists `proc_event` rows in `[from_ms, to_ms]`, newest first, optionally
    /// filtered to the given `kinds` (empty = all). Returns at most `limit` rows
    /// and a `truncated` flag set when more rows matched than were returned
    /// (M6 `ListEvents`). One extra row is fetched to detect truncation.
    pub fn list_events(
        &self,
        from_ms: i64,
        to_ms: i64,
        kinds: &[u32],
        limit: u32,
    ) -> Result<(Vec<EventListRow>, bool)> {
        let fetch = limit as i64 + 1; // one extra to detect truncation
                                      // Build an IN (...) clause for the kinds filter when present. The kinds
                                      // set is tiny (0/1) so inlining the integers is safe and keeps the
                                      // prepared statement simple.
        let kind_clause = if kinds.is_empty() {
            String::new()
        } else {
            let list = kinds
                .iter()
                .map(|k| k.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!(" AND kind IN ({list})")
        };
        let sql = format!(
            "SELECT ts_ms, kind, pid, parent_pid, session_id, image_name, exit_status
               FROM proc_event
              WHERE ts_ms >= ?1 AND ts_ms <= ?2{kind_clause}
              ORDER BY ts_ms DESC
              LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![from_ms, to_ms, fetch], |r| {
            let exit: Option<i64> = r.get(6)?;
            Ok(EventListRow {
                ts_ms: r.get(0)?,
                kind: r.get::<_, i64>(1)? as u32,
                pid: r.get::<_, i64>(2)? as u32,
                parent_pid: r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u32,
                session_id: r.get::<_, Option<i64>>(4)?.unwrap_or(0) as u32,
                image_name: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                exit_status: exit.unwrap_or(0) as i32,
                has_exit_status: exit.is_some(),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        let truncated = out.len() as i64 > limit as i64;
        out.truncate(limit as usize);
        Ok((out, truncated))
    }

    /// Records one privacy-capability usage transition (docs/phases.md M7). The
    /// ConsentStore watcher calls this when it observes an app start or stop
    /// using camera/mic/location; `ListPrivacyEvents` reads them back.
    pub fn record_privacy_event(&self, ev: &PrivacyEventRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO privacy_event (ts_ms, capability, app_id, display_name, started)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                ev.ts_ms,
                ev.capability,
                ev.app_id,
                ev.display_name,
                if ev.started { 1 } else { 0 },
            ],
        )?;
        Ok(())
    }

    /// Lists recorded privacy-capability transitions in `[from_ms, to_ms]`, most
    /// recent first, capped at `limit`. Returns `(rows, truncated)` where
    /// `truncated` is true when more rows matched than the limit (one extra row
    /// is fetched to detect this, mirroring `list_events`).
    pub fn list_privacy_events(
        &self,
        from_ms: i64,
        to_ms: i64,
        limit: u32,
    ) -> Result<(Vec<PrivacyEventRow>, bool)> {
        let fetch = limit as i64 + 1;
        let mut stmt = self.conn.prepare(
            "SELECT ts_ms, capability, app_id, display_name, started
               FROM privacy_event
              WHERE ts_ms >= ?1 AND ts_ms <= ?2
              ORDER BY ts_ms DESC
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![from_ms, to_ms, fetch], |r| {
            Ok(PrivacyEventRow {
                ts_ms: r.get(0)?,
                capability: r.get::<_, i64>(1)? as i32,
                app_id: r.get(2)?,
                display_name: r.get(3)?,
                started: r.get::<_, i64>(4)? != 0,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        let truncated = out.len() as i64 > limit as i64;
        out.truncate(limit as usize);
        Ok((out, truncated))
    }

    /// Case-insensitive search across process instances (name or pid),
    /// `proc_event` image names, and bookmark labels (M6 `Search`). `limit` caps
    /// each entity list independently.
    ///
    /// Process instances and bookmarks are served from the FTS5 indexes when the
    /// module is present (a prefix query, so `chr` matches `chrome.exe`); events
    /// and — when FTS5 is unavailable — everything, fall back to an escaped
    /// substring LIKE scan. A purely numeric query additionally matches by pid.
    /// The recent corpus is small, so LIKE scans cheaply; FTS5 is preferred per
    /// docs/phases.md M6.
    pub fn search(&self, query: &str, limit: u32) -> Result<SearchHits> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(SearchHits::default());
        }
        let pid_num: Option<i64> = q.parse::<i64>().ok();
        let lim = limit as i64;
        let use_fts = self.has_fts5();

        let processes = if use_fts {
            self.search_processes_fts(q, pid_num, lim)?
        } else {
            self.search_processes_like(q, pid_num, lim)?
        };
        // proc_event is high-churn and intentionally not FTS-indexed; a LIKE
        // scan over the retained window is cheap and keeps the write path free.
        let events = self.search_events_like(q, pid_num, lim)?;
        let bookmarks = if use_fts {
            self.search_bookmarks_fts(q, lim)?
        } else {
            self.search_bookmarks_like(q, lim)?
        };

        Ok(SearchHits {
            processes,
            events,
            bookmarks,
        })
    }

    /// Builds an FTS5 MATCH expression from a free-text query: each
    /// whitespace-separated token becomes a quoted prefix term (`"tok"*`) so a
    /// partial name matches and FTS5 special characters are treated literally.
    fn fts_prefix_query(q: &str) -> String {
        q.split_whitespace()
            .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn search_processes_fts(
        &self,
        q: &str,
        pid_num: Option<i64>,
        lim: i64,
    ) -> Result<Vec<ProcessHitRow>> {
        let match_expr = Self::fts_prefix_query(q);
        // The FTS MATCH must be the sole constraint on the FTS table, so it runs
        // in a rowid subquery; the pid arm is OR-ed against that on the base
        // table (mixing MATCH with OR on a joined row errors with "unable to use
        // function MATCH in the requested context").
        let mut stmt = self.conn.prepare(
            "SELECT id, pid, image_name, first_seen_ms, exit_seen_ms
               FROM process_instance
              WHERE id IN (SELECT rowid FROM process_fts WHERE process_fts MATCH ?1)
                 OR (?2 IS NOT NULL AND pid = ?2)
              ORDER BY (exit_seen_ms IS NULL) DESC, last_seen_ms DESC
              LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![match_expr, pid_num, lim], Self::map_process_hit)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn search_processes_like(
        &self,
        q: &str,
        pid_num: Option<i64>,
        lim: i64,
    ) -> Result<Vec<ProcessHitRow>> {
        let pattern = like_pattern(q);
        let mut stmt = self.conn.prepare(
            "SELECT id, pid, image_name, first_seen_ms, exit_seen_ms
               FROM process_instance
              WHERE image_name LIKE ?1 ESCAPE '\\' COLLATE NOCASE
                 OR (?2 IS NOT NULL AND pid = ?2)
              ORDER BY (exit_seen_ms IS NULL) DESC, last_seen_ms DESC
              LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![pattern, pid_num, lim], Self::map_process_hit)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn map_process_hit(r: &rusqlite::Row) -> rusqlite::Result<ProcessHitRow> {
        let exit: Option<i64> = r.get(4)?;
        Ok(ProcessHitRow {
            proc_row_id: r.get(0)?,
            pid: r.get::<_, i64>(1)? as u32,
            image_name: r.get(2)?,
            first_seen_ms: r.get(3)?,
            exit_seen_ms: exit.unwrap_or(0),
            live: exit.is_none(),
        })
    }

    fn search_events_like(
        &self,
        q: &str,
        pid_num: Option<i64>,
        lim: i64,
    ) -> Result<Vec<EventListRow>> {
        let pattern = like_pattern(q);
        let mut stmt = self.conn.prepare(
            "SELECT ts_ms, kind, pid, parent_pid, session_id, image_name, exit_status
               FROM proc_event
              WHERE image_name LIKE ?1 ESCAPE '\\' COLLATE NOCASE
                 OR (?2 IS NOT NULL AND pid = ?2)
              ORDER BY ts_ms DESC
              LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![pattern, pid_num, lim], |r| {
                let exit: Option<i64> = r.get(6)?;
                Ok(EventListRow {
                    ts_ms: r.get(0)?,
                    kind: r.get::<_, i64>(1)? as u32,
                    pid: r.get::<_, i64>(2)? as u32,
                    parent_pid: r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u32,
                    session_id: r.get::<_, Option<i64>>(4)?.unwrap_or(0) as u32,
                    image_name: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    exit_status: exit.unwrap_or(0) as i32,
                    has_exit_status: exit.is_some(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn search_bookmarks_fts(&self, q: &str, lim: i64) -> Result<Vec<BookmarkRow>> {
        let match_expr = Self::fts_prefix_query(q);
        let mut stmt = self.conn.prepare(
            "SELECT id, ts_ms, label, created_ms
               FROM bookmark
              WHERE id IN (SELECT rowid FROM bookmark_fts WHERE bookmark_fts MATCH ?1)
              ORDER BY ts_ms DESC
              LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![match_expr, lim], Self::map_bookmark)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn search_bookmarks_like(&self, q: &str, lim: i64) -> Result<Vec<BookmarkRow>> {
        let pattern = like_pattern(q);
        let mut stmt = self.conn.prepare(
            "SELECT id, ts_ms, label, created_ms
               FROM bookmark
              WHERE label LIKE ?1 ESCAPE '\\' COLLATE NOCASE
              ORDER BY ts_ms DESC
              LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![pattern, lim], Self::map_bookmark)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn map_bookmark(r: &rusqlite::Row) -> rusqlite::Result<BookmarkRow> {
        Ok(BookmarkRow {
            id: r.get(0)?,
            ts_ms: r.get(1)?,
            label: r.get(2)?,
            created_ms: r.get(3)?,
        })
    }

    /// Inserts an incident bookmark and returns its row id (M6 `CreateBookmark`).
    /// `created_ms` is stamped from wall clock at insert time.
    pub fn create_bookmark(&self, ts_ms: i64, label: &str) -> Result<i64> {
        let created_ms = crate::now_ms();
        self.conn.execute(
            "INSERT INTO bookmark (ts_ms, label, created_ms) VALUES (?1, ?2, ?3)",
            params![ts_ms, label, created_ms],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Lists bookmarks whose `ts_ms` falls in `[from_ms, to_ms]`, ascending by
    /// time (M6 `ListBookmarks`).
    pub fn list_bookmarks(&self, from_ms: i64, to_ms: i64) -> Result<Vec<BookmarkRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, ts_ms, label, created_ms FROM bookmark
              WHERE ts_ms >= ?1 AND ts_ms <= ?2
              ORDER BY ts_ms ASC",
        )?;
        let rows = stmt
            .query_map(params![from_ms, to_ms], |r| {
                Ok(BookmarkRow {
                    id: r.get(0)?,
                    ts_ms: r.get(1)?,
                    label: r.get(2)?,
                    created_ms: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Appends one safe-action audit row (PRD §9.22 — every prepare/execute is
    /// recorded regardless of outcome). Append-only; the log is never updated or
    /// deleted by the broker.
    pub fn record_audit(&self, a: &AuditRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO audit (ts_ms, actor, action, pid, image_name, decision, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                a.ts_ms,
                a.actor,
                a.action,
                a.pid,
                a.image_name,
                a.decision,
                a.detail
            ],
        )?;
        Ok(())
    }

    /// Reads the most recent audit rows, newest first (dev/verification helper).
    pub fn recent_audit(&self, limit: u32) -> Result<Vec<AuditRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT ts_ms, actor, action, pid, image_name, decision, detail
               FROM audit ORDER BY ts_ms DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(AuditRow {
                    ts_ms: r.get(0)?,
                    actor: r.get(1)?,
                    action: r.get(2)?,
                    pid: r.get::<_, i64>(3)? as u32,
                    image_name: r.get(4)?,
                    decision: r.get(5)?,
                    detail: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Records or extends a detected incident (docs/phases.md M8). Idempotent by
    /// `(kind, start_ms)`: a first sight inserts a new row; a re-detection of the
    /// same episode (same kind + start) updates its `end_ms`, keeps the higher
    /// `severity`/`peak_value`, and refreshes the `summary`. This lets the writer
    /// re-run detection over overlapping windows (and again at shutdown) without
    /// ever spawning duplicate incidents. Returns the incident's row id.
    pub fn upsert_incident(
        &self,
        kind: i32,
        start_ms: i64,
        end_ms: Option<i64>,
        severity: i32,
        peak_value: f64,
        summary: &str,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO incident (kind, start_ms, end_ms, severity, peak_value, summary)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(kind, start_ms) DO UPDATE SET
                 end_ms     = COALESCE(incident.end_ms, excluded.end_ms),
                 severity   = MAX(incident.severity, excluded.severity),
                 peak_value = MAX(incident.peak_value, excluded.peak_value),
                 summary    = excluded.summary",
            params![kind, start_ms, end_ms, severity, peak_value, summary],
        )?;
        let id = self.conn.query_row(
            "SELECT id FROM incident WHERE kind = ?1 AND start_ms = ?2",
            params![kind, start_ms],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    /// Lists incidents overlapping `[from_ms, to_ms]`, newest first, capped at
    /// `limit`. An incident overlaps the window when it starts at or before `to`
    /// and is either still ongoing (`end_ms IS NULL`) or ends at or after `from`.
    /// Returns `(rows, truncated)` — one extra row is fetched to set `truncated`,
    /// mirroring `list_events`.
    pub fn list_incidents(
        &self,
        from_ms: i64,
        to_ms: i64,
        limit: u32,
    ) -> Result<(Vec<IncidentRow>, bool)> {
        let fetch = limit as i64 + 1;
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, start_ms, end_ms, severity, peak_value, summary
               FROM incident
              WHERE start_ms <= ?2 AND (end_ms IS NULL OR end_ms >= ?1)
              ORDER BY start_ms DESC
              LIMIT ?3",
        )?;
        let rows = stmt
            .query_map(params![from_ms, to_ms, fetch], Self::map_incident)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = rows;
        let truncated = out.len() as i64 > limit as i64;
        out.truncate(limit as usize);
        Ok((out, truncated))
    }

    /// Fetches one incident by id (M8 `Diagnose`/`GenerateReport` resolve the
    /// window from the incident record).
    pub fn get_incident(&self, id: i64) -> Result<Option<IncidentRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT id, kind, start_ms, end_ms, severity, peak_value, summary
                   FROM incident WHERE id = ?1",
                params![id],
                Self::map_incident,
            )
            .optional()?;
        Ok(row)
    }

    fn map_incident(r: &rusqlite::Row) -> rusqlite::Result<IncidentRow> {
        Ok(IncidentRow {
            id: r.get(0)?,
            kind: r.get::<_, i64>(1)? as i32,
            start_ms: r.get(2)?,
            end_ms: r.get(3)?,
            severity: r.get::<_, i64>(4)? as i32,
            peak_value: r.get(5)?,
            summary: r.get(6)?,
        })
    }

    /// Buckets a metric series over `[from_ms, to_ms]` into at most `buckets`
    /// min/max/avg/count spans (M6 `QueryRange`). Reads matching `sample_block`s,
    /// decodes them via atlas-tsdb, and folds every in-window point into its
    /// time bucket. Empty buckets are omitted (the UI renders gaps as missing
    /// data, never zero — PRD §11.3), and buckets come out ascending by
    /// `start_ms`. `buckets` of 0 uses the server default of 500.
    ///
    /// `scope` selects the series: a `process_instance` row id for per-process
    /// metrics, or [`SYSTEM_SCOPE`] (0) for the system gauges. Mirrors the
    /// min/max-preserving decimation of [`atlas_tsdb::SeriesRing::decimate_minmax`]
    /// but adds avg/count and works over the persisted block store.
    pub fn query_range(
        &self,
        metric: Metric,
        scope: i64,
        from_ms: i64,
        to_ms: i64,
        buckets: u32,
    ) -> Result<Vec<RangeBucketRow>> {
        let n_buckets = if buckets == 0 { 500 } else { buckets } as usize;
        if to_ms <= from_ms {
            return Ok(Vec::new());
        }
        let span = (to_ms - from_ms) as u128;
        let width = (span / n_buckets as u128).max(1) as i64;

        // One accumulator per bucket index; None until a point lands there.
        struct Acc {
            start_ms: i64,
            min: f64,
            max: f64,
            sum: f64,
            count: u32,
        }
        let mut acc: Vec<Option<Acc>> = (0..n_buckets).map(|_| None).collect();

        for blk in self.read_blocks(metric, Some(scope), from_ms, to_ms)? {
            for &(ts, v) in &blk.points {
                // Half-open [from, to): the upper bound is exclusive so a bucket
                // boundary point is not double-counted.
                if ts < from_ms || ts >= to_ms {
                    continue;
                }
                let idx = ((((ts - from_ms) as u128) * n_buckets as u128) / span) as usize;
                let idx = idx.min(n_buckets - 1);
                match &mut acc[idx] {
                    Some(a) => {
                        a.min = a.min.min(v);
                        a.max = a.max.max(v);
                        a.sum += v;
                        a.count += 1;
                    }
                    slot @ None => {
                        *slot = Some(Acc {
                            start_ms: from_ms + idx as i64 * width,
                            min: v,
                            max: v,
                            sum: v,
                            count: 1,
                        });
                    }
                }
            }
        }

        Ok(acc
            .into_iter()
            .flatten()
            .map(|a| RangeBucketRow {
                start_ms: a.start_ms,
                min: a.min,
                max: a.max,
                avg: a.sum / a.count as f64,
                samples: a.count,
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // R2 rules-engine CRUD (schema v8, PRD §9.7). Rules and profiles persist
    // across restarts; the applier loop (in `serve`) reads enabled rules each
    // tick, and the AtlasRules service serves the full CRUD surface.
    // -----------------------------------------------------------------------

    /// Inserts a rule (its `id` is ignored; `created_ms` is stamped now when 0)
    /// and returns the new row id.
    pub fn create_rule(&self, r: &RuleRow) -> Result<i64> {
        let created = if r.created_ms == 0 {
            now_ms()
        } else {
            r.created_ms
        };
        self.conn.execute(
            "INSERT INTO rule
                 (name, enabled, match_image, trigger, priority_class,
                  affinity_mode, affinity_mask, eco_qos, precedence, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                r.name,
                r.enabled as i64,
                r.match_image,
                r.trigger,
                r.priority_class,
                r.affinity_mode,
                r.affinity_mask as i64,
                r.eco_qos as i64,
                r.precedence,
                created,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn map_rule(row: &rusqlite::Row) -> rusqlite::Result<RuleRow> {
        Ok(RuleRow {
            id: row.get(0)?,
            name: row.get(1)?,
            enabled: row.get::<_, i64>(2)? != 0,
            match_image: row.get(3)?,
            trigger: row.get::<_, i64>(4)? as i32,
            priority_class: row.get::<_, i64>(5)? as i32,
            affinity_mode: row.get::<_, i64>(6)? as i32,
            affinity_mask: row.get::<_, i64>(7)? as u64,
            eco_qos: row.get::<_, i64>(8)? != 0,
            precedence: row.get::<_, i64>(9)? as i32,
            created_ms: row.get(10)?,
        })
    }

    const RULE_COLS: &'static str =
        "id, name, enabled, match_image, trigger, priority_class, affinity_mode, \
         affinity_mask, eco_qos, precedence, created_ms";

    /// Fetches one rule by id.
    pub fn get_rule(&self, id: i64) -> Result<Option<RuleRow>> {
        let sql = format!("SELECT {} FROM rule WHERE id = ?1", Self::RULE_COLS);
        let row = self
            .conn
            .query_row(&sql, params![id], Self::map_rule)
            .optional()?;
        Ok(row)
    }

    /// Lists all rules, ordered by precedence descending then id ascending (the
    /// order the resolver walks for a stable, documented conflict outcome).
    pub fn list_rules(&self) -> Result<Vec<RuleRow>> {
        let sql = format!(
            "SELECT {} FROM rule ORDER BY precedence DESC, id ASC",
            Self::RULE_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], Self::map_rule)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Lists only enabled rules (the applier's per-tick input).
    pub fn list_enabled_rules(&self) -> Result<Vec<RuleRow>> {
        let sql = format!(
            "SELECT {} FROM rule WHERE enabled = 1 ORDER BY precedence DESC, id ASC",
            Self::RULE_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], Self::map_rule)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Updates a rule in place (by `id`). Returns whether a row was affected.
    pub fn update_rule(&self, r: &RuleRow) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE rule SET
                 name = ?2, enabled = ?3, match_image = ?4, trigger = ?5,
                 priority_class = ?6, affinity_mode = ?7, affinity_mask = ?8,
                 eco_qos = ?9, precedence = ?10
             WHERE id = ?1",
            params![
                r.id,
                r.name,
                r.enabled as i64,
                r.match_image,
                r.trigger,
                r.priority_class,
                r.affinity_mode,
                r.affinity_mask as i64,
                r.eco_qos as i64,
                r.precedence,
            ],
        )?;
        Ok(n > 0)
    }

    /// Deletes a rule (its profile links cascade). Returns whether a row went.
    pub fn delete_rule(&self, id: i64) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM rule WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Toggles a rule's enabled flag. Returns whether a row was affected.
    pub fn set_rule_enabled(&self, id: i64, enabled: bool) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE rule SET enabled = ?2 WHERE id = ?1",
            params![id, enabled as i64],
        )?;
        Ok(n > 0)
    }

    // -----------------------------------------------------------------------
    // R2 advanced-privacy-alerts CRUD (schema v9, PRD §9.10.3). Rules persist
    // across restarts; the ConsentStore change-watcher's evaluator (in `serve`)
    // reads enabled rules and records `fired_alert` rows. The AtlasQuery service
    // serves the full CRUD + fired-alert read surface.
    // -----------------------------------------------------------------------

    const PRIVACY_ALERT_RULE_COLS: &'static str =
        "id, name, enabled, capability, condition, threshold_seconds, created_ms";

    fn map_privacy_alert_rule(row: &rusqlite::Row) -> rusqlite::Result<PrivacyAlertRuleRow> {
        Ok(PrivacyAlertRuleRow {
            id: row.get(0)?,
            name: row.get(1)?,
            enabled: row.get::<_, i64>(2)? != 0,
            capability: row.get::<_, i64>(3)? as i32,
            condition: row.get::<_, i64>(4)? as i32,
            threshold_seconds: row.get::<_, i64>(5)? as u32,
            created_ms: row.get(6)?,
        })
    }

    /// Inserts an alert rule (its `id` is ignored; `created_ms` stamped now when
    /// 0) and returns the new row id.
    pub fn create_privacy_alert_rule(&self, r: &PrivacyAlertRuleRow) -> Result<i64> {
        let created = if r.created_ms == 0 {
            now_ms()
        } else {
            r.created_ms
        };
        self.conn.execute(
            "INSERT INTO privacy_alert_rule
                 (name, enabled, capability, condition, threshold_seconds, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                r.name,
                r.enabled as i64,
                r.capability,
                r.condition,
                r.threshold_seconds as i64,
                created,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Fetches one alert rule by id.
    pub fn get_privacy_alert_rule(&self, id: i64) -> Result<Option<PrivacyAlertRuleRow>> {
        let sql = format!(
            "SELECT {} FROM privacy_alert_rule WHERE id = ?1",
            Self::PRIVACY_ALERT_RULE_COLS
        );
        let row = self
            .conn
            .query_row(&sql, params![id], Self::map_privacy_alert_rule)
            .optional()?;
        Ok(row)
    }

    /// Lists all alert rules, newest first.
    pub fn list_privacy_alert_rules(&self) -> Result<Vec<PrivacyAlertRuleRow>> {
        let sql = format!(
            "SELECT {} FROM privacy_alert_rule ORDER BY id ASC",
            Self::PRIVACY_ALERT_RULE_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], Self::map_privacy_alert_rule)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Lists only enabled alert rules (the evaluator's input each transition).
    pub fn list_enabled_privacy_alert_rules(&self) -> Result<Vec<PrivacyAlertRuleRow>> {
        let sql = format!(
            "SELECT {} FROM privacy_alert_rule WHERE enabled = 1 ORDER BY id ASC",
            Self::PRIVACY_ALERT_RULE_COLS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map([], Self::map_privacy_alert_rule)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Updates an alert rule in place (by `id`). Returns whether a row changed.
    pub fn update_privacy_alert_rule(&self, r: &PrivacyAlertRuleRow) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE privacy_alert_rule SET
                 name = ?2, enabled = ?3, capability = ?4, condition = ?5,
                 threshold_seconds = ?6
             WHERE id = ?1",
            params![
                r.id,
                r.name,
                r.enabled as i64,
                r.capability,
                r.condition,
                r.threshold_seconds as i64,
            ],
        )?;
        Ok(n > 0)
    }

    /// Deletes an alert rule. Returns whether a row went.
    pub fn delete_privacy_alert_rule(&self, id: i64) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM privacy_alert_rule WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Records one fired alert (the evaluator's output). `id` is assigned by the
    /// store; the passed `id`/`rule_name` are ignored on insert.
    pub fn record_fired_alert(&self, a: &FiredAlertRow) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO fired_alert
                 (rule_id, ts_ms, capability, app_id, display_name, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                a.rule_id,
                a.ts_ms,
                a.capability,
                a.app_id,
                a.display_name,
                a.detail,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Lists fired alerts in `[from_ms, to_ms]`, most recent first, capped at
    /// `limit`. Returns `(rows, truncated)`; `rule_name` comes from a LEFT JOIN so
    /// a deleted rule leaves it empty rather than dropping the alert.
    pub fn list_fired_alerts(
        &self,
        from_ms: i64,
        to_ms: i64,
        limit: u32,
    ) -> Result<(Vec<FiredAlertRow>, bool)> {
        let fetch = limit as i64 + 1;
        let mut stmt = self.conn.prepare(
            "SELECT f.id, f.rule_id, COALESCE(r.name, ''), f.ts_ms, f.capability,
                    f.app_id, f.display_name, f.detail
               FROM fired_alert f
               LEFT JOIN privacy_alert_rule r ON r.id = f.rule_id
              WHERE f.ts_ms >= ?1 AND f.ts_ms <= ?2
              ORDER BY f.ts_ms DESC, f.id DESC
              LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![from_ms, to_ms, fetch], |r| {
            Ok(FiredAlertRow {
                id: r.get(0)?,
                rule_id: r.get(1)?,
                rule_name: r.get(2)?,
                ts_ms: r.get(3)?,
                capability: r.get::<_, i64>(4)? as i32,
                app_id: r.get(5)?,
                display_name: r.get(6)?,
                detail: r.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        let truncated = out.len() as i64 > limit as i64;
        out.truncate(limit as usize);
        Ok((out, truncated))
    }

    /// Inserts a profile plus its rule links, returning the new profile id.
    pub fn create_profile(
        &mut self,
        name: &str,
        power_mode: &str,
        active: bool,
        rule_ids: &[i64],
    ) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO profile (name, power_mode, active) VALUES (?1, ?2, ?3)",
            params![name, power_mode, active as i64],
        )?;
        let id = tx.last_insert_rowid();
        {
            let mut link = tx.prepare(
                "INSERT OR IGNORE INTO profile_rule (profile_id, rule_id) VALUES (?1, ?2)",
            )?;
            for rid in rule_ids {
                link.execute(params![id, rid])?;
            }
        }
        tx.commit()?;
        Ok(id)
    }

    fn profile_rule_ids(&self, profile_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT rule_id FROM profile_rule WHERE profile_id = ?1 ORDER BY rule_id")?;
        let ids = stmt
            .query_map(params![profile_id], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    /// Fetches one profile (with its rule ids) by id.
    pub fn get_profile(&self, id: i64) -> Result<Option<ProfileRow>> {
        let base = self
            .conn
            .query_row(
                "SELECT id, name, power_mode, active FROM profile WHERE id = ?1",
                params![id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)? != 0,
                    ))
                },
            )
            .optional()?;
        match base {
            Some((id, name, power_mode, active)) => Ok(Some(ProfileRow {
                id,
                name,
                power_mode,
                active,
                rule_ids: self.profile_rule_ids(id)?,
            })),
            None => Ok(None),
        }
    }

    /// Lists all profiles (each with its rule ids), ordered by id.
    pub fn list_profiles(&self) -> Result<Vec<ProfileRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, power_mode, active FROM profile ORDER BY id")?;
        let bases = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? != 0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut out = Vec::with_capacity(bases.len());
        for (id, name, power_mode, active) in bases {
            out.push(ProfileRow {
                id,
                name,
                power_mode,
                active,
                rule_ids: self.profile_rule_ids(id)?,
            });
        }
        Ok(out)
    }

    /// Updates a profile's fields and replaces its rule links. Returns whether
    /// the profile existed.
    pub fn update_profile(&mut self, p: &ProfileRow) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "UPDATE profile SET name = ?2, power_mode = ?3, active = ?4 WHERE id = ?1",
            params![p.id, p.name, p.power_mode, p.active as i64],
        )?;
        if n == 0 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM profile_rule WHERE profile_id = ?1",
            params![p.id],
        )?;
        {
            let mut link = tx.prepare(
                "INSERT OR IGNORE INTO profile_rule (profile_id, rule_id) VALUES (?1, ?2)",
            )?;
            for rid in &p.rule_ids {
                link.execute(params![p.id, rid])?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    /// Deletes a profile (its links cascade). Returns whether a row went.
    pub fn delete_profile(&self, id: i64) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM profile WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Sets a profile's active flag. Returns whether a row was affected. Rule
    /// enable/disable bundling is handled by the service, not here.
    pub fn set_profile_active(&self, id: i64, active: bool) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE profile SET active = ?2 WHERE id = ?1",
            params![id, active as i64],
        )?;
        Ok(n > 0)
    }

    // -- R3 system changes (schema v10, PRD §9.13) ------------------------------

    /// Records one detected system change (the change-detector's output). `id` is
    /// assigned by the store; `ts_ms` is stamped now when 0. Returns the new id.
    pub fn record_system_change(&self, c: &SystemChangeRow) -> Result<i64> {
        let ts = if c.ts_ms == 0 { now_ms() } else { c.ts_ms };
        self.conn.execute(
            "INSERT INTO system_change
                 (ts_ms, kind, subject, detail, publisher, responsible, reversible)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ts,
                c.kind,
                c.subject,
                c.detail,
                c.publisher,
                c.responsible,
                c.reversible as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Lists recorded system changes in `[from_ms, to_ms]`, most recent first,
    /// optionally filtered to `kinds` (empty = all), capped at `limit`. Returns
    /// `(rows, truncated)`; one extra row is fetched to detect truncation.
    pub fn list_system_changes(
        &self,
        from_ms: i64,
        to_ms: i64,
        kinds: &[i32],
        limit: u32,
    ) -> Result<(Vec<SystemChangeRow>, bool)> {
        let fetch = limit as i64 + 1;
        let kind_clause = if kinds.is_empty() {
            String::new()
        } else {
            let list = kinds
                .iter()
                .map(|k| k.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!(" AND kind IN ({list})")
        };
        let sql = format!(
            "SELECT id, ts_ms, kind, subject, detail, publisher, responsible, reversible
               FROM system_change
              WHERE ts_ms >= ?1 AND ts_ms <= ?2{kind_clause}
              ORDER BY ts_ms DESC, id DESC
              LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![from_ms, to_ms, fetch], |r| {
            Ok(SystemChangeRow {
                id: r.get(0)?,
                ts_ms: r.get(1)?,
                kind: r.get::<_, i64>(2)? as i32,
                subject: r.get(3)?,
                detail: r.get(4)?,
                publisher: r.get(5)?,
                responsible: r.get(6)?,
                reversible: r.get::<_, i64>(7)? != 0,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        let truncated = out.len() as i64 > limit as i64;
        out.truncate(limit as usize);
        Ok((out, truncated))
    }

    /// Reads the stored inventory-snapshot JSON for `kind` (the detector's "last
    /// inventory" baseline), or `None` if none is recorded yet. The blob is opaque
    /// to the store.
    pub fn get_inventory(&self, kind: &str) -> Result<Option<String>> {
        let json = self
            .conn
            .query_row(
                "SELECT json FROM inventory_snapshot WHERE kind = ?1",
                params![kind],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(json)
    }

    /// Upserts the inventory-snapshot JSON for `kind`, stamping `updated_ms` now.
    pub fn set_inventory(&self, kind: &str, json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO inventory_snapshot (kind, json, updated_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(kind) DO UPDATE SET json = excluded.json, updated_ms = excluded.updated_ms",
            params![kind, json, now_ms()],
        )?;
        Ok(())
    }

    // -- R3 crash correlation (schema v10, PRD §9.14) --------------------------

    /// Records one correlated crash (the crash-scanner's output). Idempotent on
    /// `(ts_ms, kind, subject)`: a re-scan of the same log window refreshes the
    /// fault / exception / context on the existing row rather than duplicating it.
    /// `context` is serialized to a JSON array. Returns the row id.
    pub fn record_crash(&self, c: &CrashRow) -> Result<i64> {
        let context_json =
            serde_json::to_string(&c.context).with_context(|| "serializing crash context")?;
        self.conn.execute(
            "INSERT INTO crash_record
                 (ts_ms, kind, subject, fault, exception_code, context)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(ts_ms, kind, subject) DO UPDATE SET
                 fault = excluded.fault,
                 exception_code = excluded.exception_code,
                 context = excluded.context",
            params![
                c.ts_ms,
                c.kind,
                c.subject,
                c.fault,
                c.exception_code,
                context_json,
            ],
        )?;
        let id = self.conn.query_row(
            "SELECT id FROM crash_record WHERE ts_ms = ?1 AND kind = ?2 AND subject = ?3",
            params![c.ts_ms, c.kind, c.subject],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    /// Lists recorded crashes in `[from_ms, to_ms]`, most recent first, optionally
    /// filtered to `kinds` (empty = all), capped at `limit`. Returns
    /// `(rows, truncated)`; `context` is decoded from its JSON array (a malformed
    /// blob yields an empty list rather than failing the whole read).
    pub fn list_crashes(
        &self,
        from_ms: i64,
        to_ms: i64,
        kinds: &[i32],
        limit: u32,
    ) -> Result<(Vec<CrashRow>, bool)> {
        let fetch = limit as i64 + 1;
        let kind_clause = if kinds.is_empty() {
            String::new()
        } else {
            let list = kinds
                .iter()
                .map(|k| k.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!(" AND kind IN ({list})")
        };
        let sql = format!(
            "SELECT id, ts_ms, kind, subject, fault, exception_code, context
               FROM crash_record
              WHERE ts_ms >= ?1 AND ts_ms <= ?2{kind_clause}
              ORDER BY ts_ms DESC, id DESC
              LIMIT ?3"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![from_ms, to_ms, fetch], |r| {
            let context_json: String = r.get(6)?;
            Ok(CrashRow {
                id: r.get(0)?,
                ts_ms: r.get(1)?,
                kind: r.get::<_, i64>(2)? as i32,
                subject: r.get(3)?,
                fault: r.get(4)?,
                exception_code: r.get(5)?,
                context: serde_json::from_str(&context_json).unwrap_or_default(),
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        let truncated = out.len() as i64 > limit as i64;
        out.truncate(limit as usize);
        Ok((out, truncated))
    }
}

/// Builds a case-insensitive substring LIKE pattern (`%needle%`) with the
/// query's own LIKE metacharacters (`\`, `%`, `_`) escaped, so a user typing
/// them matches literally (paired with `ESCAPE '\\'` in the SQL).
fn like_pattern(q: &str) -> String {
    let escaped = q
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// Wall-clock Unix-epoch milliseconds. Small local helper so the store can
/// stamp `created_ms`/audit timestamps without a dependency on the service.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(pid: u32) -> ProcIdentity {
        ProcIdentity {
            pid,
            create_time_100ns: 133_000_000_000_000_000 + pid as i64,
            parent_pid: 4,
            session_id: 1,
            image_name: format!("proc{pid}.exe"),
        }
    }

    #[test]
    fn upsert_is_stable_across_calls() {
        let store = Store::open_in_memory().unwrap();
        let a = store.upsert_process(&identity(100), 1_000).unwrap();
        let b = store.upsert_process(&identity(100), 2_000).unwrap();
        assert_eq!(a, b);
        let last_seen: i64 = store
            .conn
            .query_row(
                "SELECT last_seen_ms FROM process_instance WHERE id = ?1",
                params![a],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last_seen, 2_000);
    }

    #[test]
    fn pid_reuse_creates_distinct_instances() {
        let store = Store::open_in_memory().unwrap();
        let a = store.upsert_process(&identity(100), 1_000).unwrap();
        let mut reused = identity(100);
        reused.create_time_100ns += 999;
        let b = store.upsert_process(&reused, 2_000).unwrap();
        assert_ne!(a, b);
    }

    /// Builds a single-metric block for `scope` from (ts, value) points.
    fn block(metric: Metric, scope: i64, pts: &[(i64, f64)]) -> EncodedBlock {
        let mut hb = atlas_tsdb::HeadBlocks::new();
        let key = SeriesKey::new(metric, scope);
        for &(t, v) in pts {
            assert!(hb.append(key, t, v));
        }
        hb.drain_all().pop().expect("one block")
    }

    #[test]
    fn block_write_and_top_query_roundtrip() {
        let mut store = Store::open_in_memory().unwrap();
        let busy = store.upsert_process(&identity(10), 1_000).unwrap();
        let idle = store.upsert_process(&identity(20), 1_000).unwrap();

        // 1 s cadence: busy averages ~400‰ (peaks 450), idle ~10‰.
        let busy_cpu = block(
            Metric::CpuPermille,
            busy,
            &[(10_000, 400.0), (11_000, 450.0), (12_000, 350.0)],
        );
        let idle_cpu = block(
            Metric::CpuPermille,
            idle,
            &[(10_000, 10.0), (11_000, 10.0), (12_000, 10.0)],
        );
        let busy_ws = block(
            Metric::WorkingSet,
            busy,
            &[
                (10_000, (100u64 << 20) as f64),
                (11_000, (120u64 << 20) as f64),
            ],
        );
        store
            .write_batch(&[busy_cpu, idle_cpu, busy_ws], &[])
            .unwrap();

        let top = store.top_processes(0, 20_000, 10).unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].pid, 10, "busiest process sorts first");
        assert_eq!(top[0].cpu_peak_permille, 450);
        assert_eq!(top[0].working_set_peak, 120 << 20);
        assert_eq!(top[0].windows, 3, "three CPU samples contributed");
        assert!((top[0].cpu_avg_permille - 400.0).abs() < 30.0);
    }

    #[test]
    fn read_blocks_scope_filter_and_overlap() {
        let mut store = Store::open_in_memory().unwrap();
        let a = block(Metric::CpuPermille, 1, &[(1_000, 5.0), (2_000, 6.0)]);
        let b = block(Metric::CpuPermille, 2, &[(1_000, 7.0), (2_000, 8.0)]);
        let later = block(Metric::CpuPermille, 1, &[(50_000, 9.0)]);
        store.write_blocks(&[a, b, later]).unwrap();

        // Scope filter returns only scope 1's overlapping blocks.
        let s1 = store
            .read_blocks(Metric::CpuPermille, Some(1), 0, 3_000)
            .unwrap();
        assert_eq!(s1.len(), 1);
        assert_eq!(s1[0].points, vec![(1_000, 5.0), (2_000, 6.0)]);

        // No filter, window excludes the `later` block.
        let all = store
            .read_blocks(Metric::CpuPermille, None, 0, 3_000)
            .unwrap();
        assert_eq!(all.len(), 2, "both scopes, later block pruned by time");

        // Storage stat sums payload bytes.
        assert!(store.sample_storage_bytes().unwrap() > 0);
    }

    #[test]
    fn v2_migration_upgrades_v1_in_place_and_writes_self_and_gap() {
        // Build a bare v1 database: schema v1 tables, user_version pinned to 1,
        // and no v2 tables. This mimics a database written before the M1/M2
        // self-metrics slice landed.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch("PRAGMA user_version = 1;").unwrap();
        // The v2 tables must not exist yet.
        let has_self: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='self_sample'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_self, 0, "v1 database must not have self_sample");

        // Wrapping the same connection in a Store runs migrate(), which walks a
        // v1 database up through every additive migration in place, without
        // touching the existing v1 data.
        let store = Store { conn };
        store.migrate().unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 10, "migration walks v1 up to the current schema");

        store
            .write_self_sample(&SelfSampleRow {
                ts_ms: 5_000,
                cpu_permille: 3,
                working_set: 48 << 20,
                tick_duration_us_avg: 2_100,
                tick_duration_us_max: 4_800,
                ticks: 5,
            })
            .unwrap();
        store.record_gap(6_000, 2, "writer stalled").unwrap();

        let latest = store.latest_self_sample().unwrap().unwrap();
        assert_eq!(latest.cpu_permille, 3);
        assert_eq!(latest.ticks, 5);
        assert_eq!(latest.tick_duration_us_avg, 2_100);

        let dropped: i64 = store
            .conn
            .query_row(
                "SELECT dropped_windows FROM gap_event WHERE ts_ms = 6000",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dropped, 2);
    }

    #[test]
    fn fresh_database_has_no_self_sample() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.latest_self_sample().unwrap().is_none());
    }

    #[test]
    fn v3_migration_upgrades_v2_in_place() {
        // Build a v2 database: v1 + v2 schema, user_version pinned to 2, and no
        // v3 artifacts (no proc_event table, no exit_status column). This mimics
        // a database written before the M3 slice landed.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute_batch(SCHEMA_V2).unwrap();
        conn.execute_batch("PRAGMA user_version = 2;").unwrap();

        assert!(
            !column_exists(&conn, "process_instance", "exit_status").unwrap(),
            "v2 process_instance must not have exit_status"
        );
        let has_events: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='proc_event'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_events, 0, "v2 database must not have proc_event");

        // Seed a pre-existing process_instance row so we can prove the ALTER
        // preserves data (existing rows get NULL exit_status).
        conn.execute(
            "INSERT INTO process_instance
                 (pid, create_time_100ns, parent_pid, session_id, image_name,
                  first_seen_ms, last_seen_ms)
             VALUES (77, 123, 4, 1, 'pre.exe', 500, 500)",
            [],
        )
        .unwrap();

        let store = Store { conn };
        store.migrate().unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 10, "migration walks a v2 db to the current schema");
        assert!(column_exists(&store.conn, "process_instance", "exit_status").unwrap());

        // Pre-existing row survived and has a NULL exit_status.
        let (name, exit): (String, Option<i64>) = store
            .conn
            .query_row(
                "SELECT image_name, exit_status FROM process_instance WHERE pid = 77",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "pre.exe");
        assert_eq!(exit, None);

        // Migration is idempotent: a second run is a no-op.
        store.migrate().unwrap();
    }

    #[test]
    fn proc_event_batch_write_and_exit_stamping() {
        let mut store = Store::open_in_memory().unwrap();
        // A live instance for pid 4242 (as the snapshot collector would upsert).
        let id = store.upsert_process(&identity(4242), 1_000).unwrap();

        // A start event and a stop event for that pid ride a flush batch.
        let events = vec![
            ProcEventRow {
                ts_ms: 1_100,
                pid: 4242,
                kind: PROC_EVENT_START,
                parent_pid: Some(4),
                session_id: Some(1),
                image_name: Some("proc4242.exe".into()),
                exit_status: None,
            },
            ProcEventRow {
                ts_ms: 2_500,
                pid: 4242,
                kind: PROC_EVENT_STOP,
                parent_pid: None,
                session_id: None,
                image_name: None,
                exit_status: Some(0),
            },
        ];
        store.write_batch(&[], &events).unwrap();

        // Both rows landed with the right kind discriminants.
        let (starts, stops): (i64, i64) = store
            .conn
            .query_row(
                "SELECT
                     SUM(kind = 0) AS starts,
                     SUM(kind = 1) AS stops
                 FROM proc_event WHERE pid = 4242",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((starts, stops), (1, 1));

        // Exit stamping matches the live instance by pid and records the exact
        // exit ts + status.
        let stamped = store.stamp_exit_by_pid(4242, 2_500, Some(0)).unwrap();
        assert_eq!(stamped, 1, "one live instance stamped");
        let (exit_ms, exit_status): (Option<i64>, Option<i64>) = store
            .conn
            .query_row(
                "SELECT exit_seen_ms, exit_status FROM process_instance WHERE id = ?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(exit_ms, Some(2_500));
        assert_eq!(exit_status, Some(0));

        // A second stamp for the same pid finds no un-exited instance: it is a
        // no-op (models a stop-without-live-start / duplicate stop).
        let again = store.stamp_exit_by_pid(4242, 9_999, Some(1)).unwrap();
        assert_eq!(again, 0, "already-exited instance is not re-stamped");
    }

    #[test]
    fn stamp_exit_without_matching_instance_is_noop() {
        // Stop-without-start: no live instance for the pid → zero rows stamped.
        let store = Store::open_in_memory().unwrap();
        let stamped = store.stamp_exit_by_pid(1234, 5_000, Some(0)).unwrap();
        assert_eq!(stamped, 0);
    }

    #[test]
    fn block_retention_removes_fully_aged_blocks() {
        let mut store = Store::open_in_memory().unwrap();
        let id = store.upsert_process(&identity(10), 1_000).unwrap();
        // An old block (ends at 2_000) and a recent one (ends at 9_000).
        let old = block(Metric::CpuPermille, id, &[(1_000, 5.0), (2_000, 5.0)]);
        let recent = block(Metric::CpuPermille, id, &[(8_000, 7.0), (9_000, 7.0)]);
        store.write_blocks(&[old, recent]).unwrap();

        let removed = store.apply_block_retention(5_000).unwrap();
        assert_eq!(removed, 1, "only the fully-aged block is dropped");

        // The survivor is still queryable.
        let top = store.top_processes(0, 20_000, 10).unwrap();
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].windows, 2);
    }

    #[test]
    fn fresh_database_is_v10() {
        let store = Store::open_in_memory().unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 10);
    }

    fn sample_rule(name: &str, image: &str) -> RuleRow {
        RuleRow {
            id: 0,
            name: name.to_string(),
            enabled: true,
            match_image: image.to_string(),
            trigger: 1,        // WHILE_RUNNING
            priority_class: 2, // PRIORITY_BELOW_NORMAL
            affinity_mode: 0,  // unchanged
            affinity_mask: 0,
            eco_qos: true,
            precedence: 10,
            created_ms: 0,
        }
    }

    #[test]
    fn rule_crud_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .create_rule(&sample_rule("throttle chrome", "chrome.exe"))
            .unwrap();
        let got = store.get_rule(id).unwrap().unwrap();
        assert_eq!(got.match_image, "chrome.exe");
        assert!(got.enabled);
        assert!(got.created_ms > 0, "created_ms stamped");

        // Enabled list includes it; disabling removes it from the enabled list.
        assert_eq!(store.list_enabled_rules().unwrap().len(), 1);
        assert!(store.set_rule_enabled(id, false).unwrap());
        assert!(store.list_enabled_rules().unwrap().is_empty());
        assert_eq!(store.list_rules().unwrap().len(), 1, "still listed overall");

        // Update mutates fields.
        let mut upd = store.get_rule(id).unwrap().unwrap();
        upd.precedence = 99;
        upd.priority_class = 1; // idle
        assert!(store.update_rule(&upd).unwrap());
        let after = store.get_rule(id).unwrap().unwrap();
        assert_eq!(after.precedence, 99);
        assert_eq!(after.priority_class, 1);

        // Delete removes it.
        assert!(store.delete_rule(id).unwrap());
        assert!(store.get_rule(id).unwrap().is_none());
    }

    #[test]
    fn profile_crud_and_cascade() {
        let mut store = Store::open_in_memory().unwrap();
        let r1 = store.create_rule(&sample_rule("a", "a.exe")).unwrap();
        let r2 = store.create_rule(&sample_rule("b", "b.exe")).unwrap();
        let pid = store
            .create_profile("Gaming", "HighPerformance", false, &[r1, r2])
            .unwrap();

        let p = store.get_profile(pid).unwrap().unwrap();
        assert_eq!(p.name, "Gaming");
        assert_eq!(p.rule_ids, vec![r1, r2]);
        assert!(!p.active);

        // Activation flag toggles.
        assert!(store.set_profile_active(pid, true).unwrap());
        assert!(store.get_profile(pid).unwrap().unwrap().active);

        // Update replaces links.
        let mut upd = store.get_profile(pid).unwrap().unwrap();
        upd.rule_ids = vec![r2];
        upd.power_mode = "Balanced".into();
        assert!(store.update_profile(&upd).unwrap());
        let after = store.get_profile(pid).unwrap().unwrap();
        assert_eq!(after.rule_ids, vec![r2]);
        assert_eq!(after.power_mode, "Balanced");

        // Deleting a rule cascades its profile links (foreign_keys=ON).
        assert!(store.delete_rule(r2).unwrap());
        assert!(store.get_profile(pid).unwrap().unwrap().rule_ids.is_empty());

        // Deleting the profile removes it.
        assert!(store.delete_profile(pid).unwrap());
        assert!(store.get_profile(pid).unwrap().is_none());
    }

    #[test]
    fn incident_upsert_is_idempotent_and_lists_by_overlap() {
        let store = Store::open_in_memory().unwrap();
        // First sight of an ongoing CPU incident (end unknown).
        let a = store
            .upsert_incident(1, 10_000, None, 2, 88.0, "CPU high")
            .unwrap();
        // Re-detection of the same episode: same (kind, start) upserts in place,
        // extends the end, keeps the higher peak/severity, refreshes summary.
        let b = store
            .upsert_incident(1, 10_000, Some(30_000), 3, 96.0, "CPU saturated (peak 96%)")
            .unwrap();
        assert_eq!(a, b, "same (kind,start) is one row");

        let got = store.get_incident(a).unwrap().unwrap();
        assert_eq!(got.end_ms, Some(30_000));
        assert_eq!(got.severity, 3, "higher severity kept");
        assert!((got.peak_value - 96.0).abs() < 1e-9, "higher peak kept");
        assert_eq!(got.summary, "CPU saturated (peak 96%)");

        // A downgrade attempt never lowers the recorded peak/severity.
        store
            .upsert_incident(1, 10_000, Some(30_000), 1, 50.0, "stale")
            .unwrap();
        let again = store.get_incident(a).unwrap().unwrap();
        assert_eq!(again.severity, 3);
        assert!((again.peak_value - 96.0).abs() < 1e-9);

        // A distinct episode (different start) and a different kind are separate.
        store
            .upsert_incident(1, 90_000, None, 2, 87.0, "later CPU")
            .unwrap();
        store
            .upsert_incident(2, 10_000, Some(20_000), 2, 91.0, "memory")
            .unwrap();

        // Overlap query: window [0, 40_000] catches the first CPU episode
        // (10k-30k) and the memory episode (10k-20k) but not the later CPU one
        // that starts at 90k. Newest-first ordering by start_ms.
        let (rows, truncated) = store.list_incidents(0, 40_000, 10).unwrap();
        assert!(!truncated);
        assert_eq!(rows.len(), 2);
        // The ongoing incident at 90k is excluded by start_ms <= to.
        assert!(rows.iter().all(|r| r.start_ms <= 40_000));

        // An ongoing incident (end NULL) that started before the window still
        // shows up (it overlaps any later window).
        let (rows2, _) = store.list_incidents(100_000, 200_000, 10).unwrap();
        assert!(
            rows2
                .iter()
                .any(|r| r.start_ms == 90_000 && r.end_ms.is_none()),
            "ongoing incident overlaps a later window"
        );

        // Limit forces truncation.
        let (limited, trunc) = store.list_incidents(0, 200_000, 1).unwrap();
        assert_eq!(limited.len(), 1);
        assert!(trunc);
    }

    #[test]
    fn list_events_filters_kinds_and_truncates() {
        let mut store = Store::open_in_memory().unwrap();
        let events = vec![
            ProcEventRow {
                ts_ms: 1_000,
                pid: 10,
                kind: PROC_EVENT_START,
                parent_pid: Some(4),
                session_id: Some(1),
                image_name: Some("a.exe".into()),
                exit_status: None,
            },
            ProcEventRow {
                ts_ms: 2_000,
                pid: 10,
                kind: PROC_EVENT_STOP,
                parent_pid: None,
                session_id: None,
                image_name: None,
                exit_status: Some(0),
            },
            ProcEventRow {
                ts_ms: 3_000,
                pid: 11,
                kind: PROC_EVENT_START,
                parent_pid: Some(4),
                session_id: Some(1),
                image_name: Some("b.exe".into()),
                exit_status: None,
            },
        ];
        store.write_batch(&[], &events).unwrap();

        // All kinds, generous limit: three rows, newest first, not truncated.
        let (rows, truncated) = store.list_events(0, 10_000, &[], 100).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(!truncated);
        assert_eq!(rows[0].ts_ms, 3_000, "newest first");

        // Only stops.
        let (stops, _) = store
            .list_events(0, 10_000, &[PROC_EVENT_STOP as u32], 100)
            .unwrap();
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0].pid, 10);
        assert_eq!(stops[0].exit_status, 0);
        assert!(stops[0].has_exit_status);

        // Limit forces truncation.
        let (limited, truncated) = store.list_events(0, 10_000, &[], 2).unwrap();
        assert_eq!(limited.len(), 2);
        assert!(truncated, "more rows existed than the limit");
    }

    #[test]
    fn search_matches_name_pid_and_bookmark() {
        let mut store = Store::open_in_memory().unwrap();
        store.upsert_process(&identity(4242), 1_000).unwrap();
        store
            .write_batch(
                &[],
                &[ProcEventRow {
                    ts_ms: 5_000,
                    pid: 777,
                    kind: PROC_EVENT_START,
                    parent_pid: Some(4),
                    session_id: Some(1),
                    image_name: Some("chrome.exe".into()),
                    exit_status: None,
                }],
            )
            .unwrap();
        store.create_bookmark(9_000, "chrome spike").unwrap();

        // Name substring hits the live instance (proc4242.exe) — case-insensitive.
        let hits = store.search("PROC4242", 50).unwrap();
        assert!(
            hits.processes.iter().any(|p| p.pid == 4242),
            "process instance matched by name"
        );

        // "chrome" hits the event image name and the bookmark label.
        let hits = store.search("chrome", 50).unwrap();
        assert!(hits.events.iter().any(|e| e.pid == 777));
        assert!(hits.bookmarks.iter().any(|b| b.label == "chrome spike"));

        // Numeric query matches pid.
        let hits = store.search("777", 50).unwrap();
        assert!(hits.events.iter().any(|e| e.pid == 777));
    }

    #[test]
    fn bookmarks_roundtrip_and_range() {
        let store = Store::open_in_memory().unwrap();
        let a = store.create_bookmark(1_000, "one").unwrap();
        let b = store.create_bookmark(5_000, "two").unwrap();
        assert_ne!(a, b);

        let all = store.list_bookmarks(0, 10_000).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].ts_ms, 1_000, "ordered by ts ascending");

        let windowed = store.list_bookmarks(2_000, 10_000).unwrap();
        assert_eq!(windowed.len(), 1);
        assert_eq!(windowed[0].label, "two");
    }

    #[test]
    fn audit_rows_are_appended() {
        let store = Store::open_in_memory().unwrap();
        store
            .record_audit(&AuditRow {
                ts_ms: 1_000,
                actor: "local-ui".into(),
                action: "TERMINATE".into(),
                pid: 42,
                image_name: "lsass.exe".into(),
                decision: "PREPARE_DENIED".into(),
                detail: "protected-critical".into(),
            })
            .unwrap();
        store
            .record_audit(&AuditRow {
                ts_ms: 2_000,
                actor: "local-ui".into(),
                action: "SUSPEND".into(),
                pid: 99,
                image_name: "notepad.exe".into(),
                decision: "EXECUTE_OK".into(),
                detail: "suspended".into(),
            })
            .unwrap();
        let n: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
        // recent_audit returns newest first with the text fields intact.
        let recent = store.recent_audit(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].action, "SUSPEND");
        assert_eq!(recent[0].decision, "EXECUTE_OK");
        assert_eq!(recent[1].image_name, "lsass.exe");
    }

    #[test]
    fn query_range_buckets_with_gaps_and_stats() {
        let mut store = Store::open_in_memory().unwrap();
        let id = store.upsert_process(&identity(50), 1_000).unwrap();
        // Over [0,10000) with 3 buckets (~3333 ms each): the three early points
        // (0/1000/2000) fall in bucket 0, the two late points (8000/9000) in
        // bucket 2; bucket 1 is empty and must be omitted. A spike of 90 lives
        // among the early low values and must survive as the bucket max.
        let cpu = block(
            Metric::CpuPermille,
            id,
            &[
                (0, 10.0),
                (1_000, 90.0),
                (2_000, 20.0),
                (8_000, 30.0),
                (9_000, 50.0),
            ],
        );
        store.write_blocks(&[cpu]).unwrap();

        let buckets = store
            .query_range(Metric::CpuPermille, id, 0, 10_000, 3)
            .unwrap();
        // Only two non-empty buckets are returned (the empty middle one omitted).
        assert_eq!(buckets.len(), 2, "empty buckets omitted");
        // First bucket spans the three early points: min 10, max 90, avg 40.
        assert_eq!(buckets[0].samples, 3);
        assert_eq!(buckets[0].min, 10.0);
        assert_eq!(buckets[0].max, 90.0, "spike preserved");
        assert!((buckets[0].avg - 40.0).abs() < 1e-9);
        // Second bucket: the two late points, avg 40.
        assert_eq!(buckets[1].samples, 2);
        assert_eq!(buckets[1].min, 30.0);
        assert_eq!(buckets[1].max, 50.0);
        // Buckets come out ascending by start_ms.
        assert!(buckets[0].start_ms < buckets[1].start_ms);

        // System scope + empty range degenerate cleanly.
        assert!(store
            .query_range(Metric::SysCpuPermille, atlas_tsdb::SYSTEM_SCOPE, 0, 0, 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn search_prefers_fts_when_available() {
        // Documents the FTS5 outcome: the bundled sqlite has FTS5, so prefix
        // matching works ("chr" → chrome.exe) — a capability LIKE cannot give.
        let store = Store::open_in_memory().unwrap();
        assert!(store.has_fts5(), "bundled sqlite must ship FTS5");
        store.upsert_process(&identity(4242), 1_000).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO process_instance
                     (pid, create_time_100ns, parent_pid, session_id, image_name,
                      first_seen_ms, last_seen_ms)
                 VALUES (555, 7, 4, 1, 'chrome.exe', 100, 100)",
                [],
            )
            .unwrap();
        // Prefix hit via FTS.
        let hits = store.search("chro", 50).unwrap();
        assert!(
            hits.processes.iter().any(|p| p.image_name == "chrome.exe"),
            "prefix match found chrome.exe"
        );
    }

    #[test]
    fn privacy_event_round_trip_and_range() {
        let store = Store::open_in_memory().unwrap();
        // Three events: a start, its stop, and one outside the query window.
        store
            .record_privacy_event(&PrivacyEventRow {
                ts_ms: 1_000,
                capability: 1, // camera
                app_id: "app.a".into(),
                display_name: "App A".into(),
                started: true,
            })
            .unwrap();
        store
            .record_privacy_event(&PrivacyEventRow {
                ts_ms: 2_000,
                capability: 1,
                app_id: "app.a".into(),
                display_name: "App A".into(),
                started: false,
            })
            .unwrap();
        store
            .record_privacy_event(&PrivacyEventRow {
                ts_ms: 9_999,
                capability: 2, // microphone, outside window
                app_id: "app.b".into(),
                display_name: "App B".into(),
                started: true,
            })
            .unwrap();

        // Window [0, 5000] captures the first two, newest-first.
        let (rows, truncated) = store.list_privacy_events(0, 5_000, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(!truncated);
        assert_eq!(rows[0].ts_ms, 2_000);
        assert!(!rows[0].started);
        assert_eq!(rows[1].ts_ms, 1_000);
        assert!(rows[1].started);

        // Limit smaller than the matches flags truncation.
        let (rows, truncated) = store.list_privacy_events(0, 10_000, 1).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(truncated);
        // Newest overall is the mic event at 9_999.
        assert_eq!(rows[0].ts_ms, 9_999);
        assert_eq!(rows[0].capability, 2);
    }

    #[test]
    fn privacy_alert_rule_crud() {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .create_privacy_alert_rule(&PrivacyAlertRuleRow {
                id: 0,
                name: "mic any-use".into(),
                enabled: true,
                capability: 2, // microphone
                condition: 1,  // ALERT_ANY_USE
                threshold_seconds: 0,
                created_ms: 0, // stamped now
            })
            .unwrap();
        assert!(id > 0);

        let got = store.get_privacy_alert_rule(id).unwrap().unwrap();
        assert_eq!(got.name, "mic any-use");
        assert!(got.created_ms > 0, "created_ms stamped");

        // Update: disable + switch to ALERT_LONGER_THAN with a threshold.
        let mut updated = got.clone();
        updated.enabled = false;
        updated.condition = 5;
        updated.threshold_seconds = 30;
        assert!(store.update_privacy_alert_rule(&updated).unwrap());
        assert!(store.list_enabled_privacy_alert_rules().unwrap().is_empty());
        assert_eq!(store.list_privacy_alert_rules().unwrap().len(), 1);

        assert!(store.delete_privacy_alert_rule(id).unwrap());
        assert!(store.list_privacy_alert_rules().unwrap().is_empty());
    }

    #[test]
    fn fired_alert_records_and_reads_with_rule_name() {
        let store = Store::open_in_memory().unwrap();
        let rule_id = store
            .create_privacy_alert_rule(&PrivacyAlertRuleRow {
                id: 0,
                name: "cam background".into(),
                enabled: true,
                capability: 1,
                condition: 2,
                threshold_seconds: 0,
                created_ms: 1,
            })
            .unwrap();
        store
            .record_fired_alert(&FiredAlertRow {
                id: 0,
                rule_id,
                rule_name: String::new(), // ignored on insert
                ts_ms: 5_000,
                capability: 1,
                app_id: "C:#cam.exe".into(),
                display_name: "cam.exe".into(),
                detail: "camera used in the background".into(),
            })
            .unwrap();

        let (rows, truncated) = store.list_fired_alerts(0, 10_000, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!truncated);
        assert_eq!(rows[0].rule_name, "cam background", "name joined from rule");
        assert_eq!(rows[0].detail, "camera used in the background");

        // Deleting the rule leaves the fired alert but blanks the joined name.
        store.delete_privacy_alert_rule(rule_id).unwrap();
        let (rows, _) = store.list_fired_alerts(0, 10_000, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rule_name, "");
    }

    // -- R3 forensics (schema v10) ---------------------------------------------

    fn sys_change(ts_ms: i64, kind: i32, subject: &str) -> SystemChangeRow {
        SystemChangeRow {
            id: 0,
            ts_ms,
            kind,
            subject: subject.into(),
            detail: format!("{subject} changed"),
            publisher: "Acme".into(),
            responsible: String::new(),
            reversible: false,
        }
    }

    #[test]
    fn system_changes_record_and_range_filter() {
        let store = Store::open_in_memory().unwrap();
        store
            .record_system_change(&sys_change(1_000, 1, "AppA"))
            .unwrap();
        store
            .record_system_change(&sys_change(2_000, 8, "SvcB"))
            .unwrap();
        store
            .record_system_change(&sys_change(3_000, 10, "RunC"))
            .unwrap();

        // Full range, newest first.
        let (rows, truncated) = store.list_system_changes(0, 10_000, &[], 10).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(!truncated);
        assert_eq!(rows[0].subject, "RunC", "newest first");
        assert_eq!(rows[2].subject, "AppA");
        assert!(rows[0].id > 0, "store assigns id");

        // Kind filter.
        let (svc, _) = store.list_system_changes(0, 10_000, &[8], 10).unwrap();
        assert_eq!(svc.len(), 1);
        assert_eq!(svc[0].kind, 8);

        // Window filter.
        let (win, _) = store.list_system_changes(1_500, 2_500, &[], 10).unwrap();
        assert_eq!(win.len(), 1);
        assert_eq!(win[0].subject, "SvcB");
    }

    #[test]
    fn system_changes_truncation_flag() {
        let store = Store::open_in_memory().unwrap();
        for i in 0..5 {
            store
                .record_system_change(&sys_change(1_000 + i, 1, "App"))
                .unwrap();
        }
        let (rows, truncated) = store.list_system_changes(0, 10_000, &[], 3).unwrap();
        assert_eq!(rows.len(), 3);
        assert!(truncated, "more rows than the limit");
    }

    #[test]
    fn inventory_snapshot_get_set_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        assert!(
            store.get_inventory("full").unwrap().is_none(),
            "empty at first"
        );
        store.set_inventory("full", r#"{"apps":[]}"#).unwrap();
        assert_eq!(
            store.get_inventory("full").unwrap().as_deref(),
            Some(r#"{"apps":[]}"#)
        );
        // Upsert replaces.
        store.set_inventory("full", r#"{"apps":[1]}"#).unwrap();
        assert_eq!(
            store.get_inventory("full").unwrap().as_deref(),
            Some(r#"{"apps":[1]}"#)
        );
    }

    #[test]
    fn crashes_record_context_roundtrip_and_idempotent() {
        let store = Store::open_in_memory().unwrap();
        let crash = CrashRow {
            id: 0,
            ts_ms: 5_000,
            kind: 1, // APP_CRASH
            subject: "app.exe".into(),
            fault: "app.dll".into(),
            exception_code: "0xc0000005".into(),
            context: vec![
                "peak memory 82% in the 5 min before".into(),
                "'Acme' app_updated 2h before this crash (correlation, not proof)".into(),
            ],
        };
        let id1 = store.record_crash(&crash).unwrap();

        let (rows, _) = store.list_crashes(0, 10_000, &[], 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].context.len(), 2, "context JSON round-trips");
        assert!(rows[0].context[1].contains("not proof"));

        // Re-scan of the same (ts, kind, subject) refreshes rather than dupes.
        let mut refreshed = crash.clone();
        refreshed.context = vec!["updated context".into()];
        let id2 = store.record_crash(&refreshed).unwrap();
        assert_eq!(id1, id2, "idempotent on (ts_ms, kind, subject)");
        let (rows, _) = store.list_crashes(0, 10_000, &[], 10).unwrap();
        assert_eq!(rows.len(), 1, "no duplicate row");
        assert_eq!(rows[0].context, vec!["updated context".to_string()]);
    }

    #[test]
    fn crashes_kind_and_window_filter() {
        let store = Store::open_in_memory().unwrap();
        let mk = |ts: i64, kind: i32, subj: &str| CrashRow {
            id: 0,
            ts_ms: ts,
            kind,
            subject: subj.into(),
            fault: String::new(),
            exception_code: String::new(),
            context: vec![],
        };
        store.record_crash(&mk(1_000, 1, "a.exe")).unwrap();
        store.record_crash(&mk(2_000, 3, "BugCheck")).unwrap();
        store.record_crash(&mk(3_000, 1, "b.exe")).unwrap();

        let (bug, _) = store.list_crashes(0, 10_000, &[3], 10).unwrap();
        assert_eq!(bug.len(), 1);
        assert_eq!(bug[0].subject, "BugCheck");

        let (all, _) = store.list_crashes(0, 10_000, &[], 10).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].ts_ms, 3_000, "newest first");
    }
}
