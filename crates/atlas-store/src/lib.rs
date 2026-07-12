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

#[derive(Debug, Clone)]
pub struct TopProcessRow {
    pub pid: u32,
    pub image_name: String,
    pub cpu_avg_permille: f64,
    pub cpu_peak_permille: u32,
    pub working_set_peak: u64,
    pub windows: u32,
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
        conn.busy_timeout(Duration::from_secs(5))?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self> {
        let store = Store {
            conn: Connection::open_in_memory()?,
        };
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
        Ok(())
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
        assert_eq!(version, 4, "migration walks v1 up to the current schema");

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
        assert_eq!(version, 4, "migration walks a v2 db to the current schema");
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
    fn fresh_database_is_v4() {
        let store = Store::open_in_memory().unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 4);
    }
}
