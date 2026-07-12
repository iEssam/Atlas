//! SQLite-backed local store (tech-stack.md §4.2).
//!
//! Holds entities and events. Per-process samples are stored here *as
//! window aggregates* only until the chunked TSDB lands (docs/phases.md,
//! M-TSDB) — never one row per second per process, which would violate the
//! disk write-amplification budget (PRD §12.4).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

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

#[derive(Debug, Clone)]
pub struct ProcAggregate {
    pub proc_row_id: i64,
    pub cpu_avg_permille: u32,
    pub cpu_max_permille: u32,
    pub working_set_max: u64,
    pub private_bytes_max: u64,
    pub read_bps_avg: u64,
    pub write_bps_avg: u64,
    pub handles_last: u32,
    pub threads_last: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct SysSampleRow {
    pub ts_ms: i64,
    pub cpu_permille: u32,
    pub mem_used: u64,
    pub mem_total: u64,
    pub commit_used: u64,
    pub commit_limit: u64,
    pub process_count: u32,
    pub thread_count: u32,
    pub handle_count: u32,
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
    /// `proc_events` are the raw ETW start/stop rows drained during the window;
    /// they ride the same transaction so an event and its window land together.
    pub fn write_batch(
        &mut self,
        agg_ts_ms: i64,
        window_secs: u32,
        sys: &[SysSampleRow],
        aggs: &[ProcAggregate],
        proc_events: &[ProcEventRow],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut sys_stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO sys_sample
                     (ts_ms, cpu_permille, mem_used, mem_total, commit_used,
                      commit_limit, process_count, thread_count, handle_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for s in sys {
                sys_stmt.execute(params![
                    s.ts_ms,
                    s.cpu_permille,
                    s.mem_used as i64,
                    s.mem_total as i64,
                    s.commit_used as i64,
                    s.commit_limit as i64,
                    s.process_count,
                    s.thread_count,
                    s.handle_count
                ])?;
            }

            let mut agg_stmt = tx.prepare_cached(
                "INSERT INTO proc_sample
                     (ts_ms, window_secs, proc_id, cpu_avg_permille, cpu_max_permille,
                      working_set_max, private_bytes_max, read_bps_avg, write_bps_avg,
                      handles_last, threads_last)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;
            for a in aggs {
                agg_stmt.execute(params![
                    agg_ts_ms,
                    window_secs,
                    a.proc_row_id,
                    a.cpu_avg_permille,
                    a.cpu_max_permille,
                    a.working_set_max as i64,
                    a.private_bytes_max as i64,
                    a.read_bps_avg as i64,
                    a.write_bps_avg as i64,
                    a.handles_last,
                    a.threads_last
                ])?;
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
    /// removed.
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

    pub fn top_processes(&self, since_ms: i64, limit: u32) -> Result<Vec<TopProcessRow>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT pi.pid, pi.image_name,
                    AVG(ps.cpu_avg_permille) AS cpu_avg,
                    MAX(ps.cpu_max_permille) AS cpu_peak,
                    MAX(ps.working_set_max) AS ws_peak,
                    COUNT(*) AS windows
             FROM proc_sample ps
             JOIN process_instance pi ON pi.id = ps.proc_id
             WHERE ps.ts_ms >= ?1
             GROUP BY ps.proc_id, pi.pid, pi.image_name
             ORDER BY cpu_avg DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![since_ms, limit], |r| {
            Ok(TopProcessRow {
                pid: r.get(0)?,
                image_name: r.get(1)?,
                cpu_avg_permille: r.get(2)?,
                cpu_peak_permille: r.get(3)?,
                working_set_peak: r.get::<_, i64>(4)? as u64,
                windows: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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

    #[test]
    fn batch_write_and_top_query_roundtrip() {
        let mut store = Store::open_in_memory().unwrap();
        let busy = store.upsert_process(&identity(10), 1_000).unwrap();
        let idle = store.upsert_process(&identity(20), 1_000).unwrap();

        let agg = |id: i64, cpu: u32| ProcAggregate {
            proc_row_id: id,
            cpu_avg_permille: cpu,
            cpu_max_permille: cpu + 50,
            working_set_max: 100 << 20,
            private_bytes_max: 80 << 20,
            read_bps_avg: 1024,
            write_bps_avg: 512,
            handles_last: 42,
            threads_last: 7,
        };
        let sys = SysSampleRow {
            ts_ms: 10_000,
            cpu_permille: 300,
            mem_used: 8 << 30,
            mem_total: 16 << 30,
            commit_used: 10 << 30,
            commit_limit: 32 << 30,
            process_count: 2,
            thread_count: 14,
            handle_count: 84,
        };
        store
            .write_batch(10_000, 15, &[sys], &[agg(busy, 400), agg(idle, 10)], &[])
            .unwrap();

        let top = store.top_processes(0, 10).unwrap();
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].pid, 10, "busiest process sorts first");
        assert_eq!(top[0].cpu_peak_permille, 450);
        assert_eq!(top[0].windows, 1);
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
        assert_eq!(version, 3, "migration walks v1 up to the current schema");

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
        assert_eq!(version, 3, "migration bumps user_version to 3");
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
    fn fresh_database_is_v3() {
        let store = Store::open_in_memory().unwrap();
        let version: i64 = store
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 3);
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
        store.write_batch(2_500, 15, &[], &[], &events).unwrap();

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
    fn retention_removes_old_rows_only() {
        let mut store = Store::open_in_memory().unwrap();
        let id = store.upsert_process(&identity(10), 1_000).unwrap();
        let agg = ProcAggregate {
            proc_row_id: id,
            cpu_avg_permille: 1,
            cpu_max_permille: 1,
            working_set_max: 1,
            private_bytes_max: 1,
            read_bps_avg: 0,
            write_bps_avg: 0,
            handles_last: 1,
            threads_last: 1,
        };
        let sys = |ts: i64| SysSampleRow {
            ts_ms: ts,
            cpu_permille: 0,
            mem_used: 0,
            mem_total: 1,
            commit_used: 0,
            commit_limit: 1,
            process_count: 1,
            thread_count: 1,
            handle_count: 1,
        };
        store
            .write_batch(1_000, 15, &[sys(1_000)], std::slice::from_ref(&agg), &[])
            .unwrap();
        store
            .write_batch(9_000, 15, &[sys(9_000)], &[agg], &[])
            .unwrap();

        let (p, s) = store.apply_retention(5_000).unwrap();
        assert_eq!((p, s), (1, 1));
        assert_eq!(store.top_processes(0, 10).unwrap()[0].windows, 1);
    }
}
