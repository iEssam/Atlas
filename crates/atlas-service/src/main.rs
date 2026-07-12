//! Atlas service host (tech-stack.md §4.1).
//!
//! Dev console mode today: `top` / `snapshot` / `record` / `db-top`
//! subcommands exercise the collection → storage path end-to-end.
//! Windows-service mode, IPC server, and ETW collectors arrive at
//! milestones M3/M4/M9 (docs/phases.md).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand};

use atlas_collectors::{ProcKey, ProcSample, SampleSet, Sampler};
use atlas_store::{ProcAggregate, ProcIdentity, Store, SysSampleRow};

#[derive(Parser)]
#[command(
    name = "atlas-service",
    version,
    about = "System Atlas collection service (dev console mode)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print one full process snapshot as JSON.
    Snapshot,
    /// Live top-style view of processes by CPU.
    Top {
        /// Sampling interval in seconds.
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
        /// Number of refreshes before exiting (0 = run until Ctrl+C).
        #[arg(long, default_value_t = 0)]
        count: u32,
        /// Rows to display.
        #[arg(long, default_value_t = 25)]
        limit: usize,
    },
    /// Sample continuously and record aggregated windows to SQLite.
    Record {
        /// Database path (default: %LOCALAPPDATA%\SystemAtlas\dev\atlas.db).
        #[arg(long)]
        db: Option<PathBuf>,
        /// Sampling interval in seconds.
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
        /// Aggregation window flushed per transaction.
        #[arg(long, default_value_t = 15)]
        flush_secs: u64,
        /// Stop after this many seconds (default: run until Ctrl+C).
        #[arg(long)]
        duration: Option<u64>,
    },
    /// Query recorded data: top processes by average CPU.
    DbTop {
        /// Database path (default: %LOCALAPPDATA%\SystemAtlas\dev\atlas.db).
        #[arg(long)]
        db: Option<PathBuf>,
        /// Look-back window in minutes.
        #[arg(long, default_value_t = 15)]
        minutes: u64,
        #[arg(long, default_value_t = 15)]
        limit: u32,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match Cli::parse().cmd {
        Cmd::Snapshot => cmd_snapshot(),
        Cmd::Top {
            interval,
            count,
            limit,
        } => cmd_top(interval, count, limit),
        Cmd::Record {
            db,
            interval,
            flush_secs,
            duration,
        } => cmd_record(
            db.unwrap_or_else(default_db_path),
            interval,
            flush_secs,
            duration,
        ),
        Cmd::DbTop { db, minutes, limit } => {
            cmd_db_top(db.unwrap_or_else(default_db_path), minutes, limit)
        }
    }
}

fn default_db_path() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("SystemAtlas")
        .join("dev")
        .join("atlas.db")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn install_ctrlc() -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    if let Err(e) = ctrlc::set_handler(move || s.store(true, Ordering::SeqCst)) {
        tracing::warn!("Ctrl+C handler unavailable: {e}");
    }
    stop
}

fn cmd_snapshot() -> Result<()> {
    let procs = atlas_collectors::snapshot_processes()?;
    println!("{}", serde_json::to_string_pretty(&procs)?);
    Ok(())
}

fn cmd_top(interval: f64, count: u32, limit: usize) -> Result<()> {
    let stop = install_ctrlc();
    let mut sampler = Sampler::new()?;
    let mut iterations = 0u32;

    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs_f64(interval.max(0.1)));
        let set = sampler.sample()?;
        render_top(&set, limit);
        iterations += 1;
        if count != 0 && iterations >= count {
            break;
        }
    }
    Ok(())
}

fn render_top(set: &SampleSet, limit: usize) {
    let s = &set.system;
    println!();
    println!(
        "CPU {:>5.1}%  |  Memory {:.1}/{:.1} GB  |  Commit {:.1}/{:.1} GB  |  {} processes, {} threads, {} handles",
        s.cpu_permille as f64 / 10.0,
        gb(s.mem_used),
        gb(s.mem_total),
        gb(s.commit_used),
        gb(s.commit_limit),
        s.process_count,
        s.thread_count,
        s.handle_count
    );
    println!(
        "{:>7} {:<30} {:>6} {:>9} {:>9} {:>11} {:>11} {:>5} {:>7}",
        "PID", "NAME", "CPU%", "WS MB", "PRIV MB", "READ/s", "WRITE/s", "THR", "HANDLE"
    );

    let mut rows: Vec<&ProcSample> = set.processes.iter().collect();
    rows.sort_by(|a, b| {
        b.cpu_permille
            .cmp(&a.cpu_permille)
            .then(b.working_set.cmp(&a.working_set))
    });

    for p in rows.into_iter().take(limit) {
        println!(
            "{:>7} {:<30} {:>6.1} {:>9.1} {:>9.1} {:>11} {:>11} {:>5} {:>7}",
            p.key.pid,
            truncate(&p.image_name, 30),
            p.cpu_permille as f64 / 10.0,
            mb(p.working_set),
            mb(p.private_bytes),
            rate(p.read_bps),
            rate(p.write_bps),
            p.thread_count,
            p.handle_count
        );
    }
}

struct AggAcc {
    identity: ProcIdentity,
    n: u64,
    cpu_sum: u64,
    cpu_max: u32,
    ws_max: u64,
    priv_max: u64,
    read_bps_sum: u64,
    write_bps_sum: u64,
    handles_last: u32,
    threads_last: u32,
}

impl AggAcc {
    fn new(p: &ProcSample) -> Self {
        Self {
            identity: ProcIdentity {
                pid: p.key.pid,
                create_time_100ns: p.key.create_time_100ns,
                parent_pid: p.parent_pid,
                session_id: p.session_id,
                image_name: p.image_name.clone(),
            },
            n: 0,
            cpu_sum: 0,
            cpu_max: 0,
            ws_max: 0,
            priv_max: 0,
            read_bps_sum: 0,
            write_bps_sum: 0,
            handles_last: 0,
            threads_last: 0,
        }
    }

    fn update(&mut self, p: &ProcSample) {
        self.n += 1;
        self.cpu_sum += p.cpu_permille as u64;
        self.cpu_max = self.cpu_max.max(p.cpu_permille);
        self.ws_max = self.ws_max.max(p.working_set);
        self.priv_max = self.priv_max.max(p.private_bytes);
        self.read_bps_sum += p.read_bps;
        self.write_bps_sum += p.write_bps;
        self.handles_last = p.handle_count;
        self.threads_last = p.thread_count;
    }

    fn finish(&self, proc_row_id: i64) -> ProcAggregate {
        let n = self.n.max(1);
        ProcAggregate {
            proc_row_id,
            cpu_avg_permille: (self.cpu_sum / n) as u32,
            cpu_max_permille: self.cpu_max,
            working_set_max: self.ws_max,
            private_bytes_max: self.priv_max,
            read_bps_avg: self.read_bps_sum / n,
            write_bps_avg: self.write_bps_sum / n,
            handles_last: self.handles_last,
            threads_last: self.threads_last,
        }
    }
}

const RETENTION_HOURS: i64 = 72;

fn cmd_record(
    db_path: PathBuf,
    interval: f64,
    flush_secs: u64,
    duration: Option<u64>,
) -> Result<()> {
    let stop = install_ctrlc();
    let mut store = Store::open(&db_path)?;
    let mut sampler = Sampler::new()?;
    tracing::info!(db = %db_path.display(), interval, flush_secs, "recording started (Ctrl+C to stop)");

    let started = Instant::now();
    let flush_every = Duration::from_secs(flush_secs.max(2));
    let mut last_flush = Instant::now();

    let mut id_cache: HashMap<ProcKey, i64> = HashMap::new();
    let mut accs: HashMap<ProcKey, AggAcc> = HashMap::new();
    let mut sys_buf: Vec<SysSampleRow> = Vec::new();
    let mut flushed_windows = 0u64;
    let mut flushed_proc_rows = 0u64;

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if let Some(secs) = duration {
            if started.elapsed() >= Duration::from_secs(secs) {
                break;
            }
        }
        std::thread::sleep(Duration::from_secs_f64(interval.max(0.25)));

        let set = sampler.sample()?;
        sys_buf.push(SysSampleRow {
            ts_ms: set.ts_ms,
            cpu_permille: set.system.cpu_permille,
            mem_used: set.system.mem_used,
            mem_total: set.system.mem_total,
            commit_used: set.system.commit_used,
            commit_limit: set.system.commit_limit,
            process_count: set.system.process_count,
            thread_count: set.system.thread_count,
            handle_count: set.system.handle_count,
        });
        for p in &set.processes {
            accs.entry(p.key)
                .or_insert_with(|| AggAcc::new(p))
                .update(p);
        }
        for key in &set.exited {
            if let Some(row_id) = id_cache.remove(key) {
                store.mark_exited(row_id, set.ts_ms)?;
            }
        }

        if last_flush.elapsed() >= flush_every {
            let window_secs = last_flush.elapsed().as_secs().max(1) as u32;
            let (w, r) = flush(
                &mut store,
                &mut id_cache,
                &mut accs,
                &mut sys_buf,
                window_secs,
            )?;
            flushed_windows += w;
            flushed_proc_rows += r;
            last_flush = Instant::now();
        }
    }

    let window_secs = last_flush.elapsed().as_secs().max(1) as u32;
    let (w, r) = flush(
        &mut store,
        &mut id_cache,
        &mut accs,
        &mut sys_buf,
        window_secs,
    )?;
    flushed_windows += w;
    flushed_proc_rows += r;

    let cutoff = now_ms() - RETENTION_HOURS * 3_600_000;
    let (pruned_proc, pruned_sys) = store.apply_retention(cutoff)?;
    tracing::info!(
        flushed_windows,
        flushed_proc_rows,
        pruned_proc,
        pruned_sys,
        "recording stopped"
    );
    Ok(())
}

fn flush(
    store: &mut Store,
    id_cache: &mut HashMap<ProcKey, i64>,
    accs: &mut HashMap<ProcKey, AggAcc>,
    sys_buf: &mut Vec<SysSampleRow>,
    window_secs: u32,
) -> Result<(u64, u64)> {
    if accs.is_empty() && sys_buf.is_empty() {
        return Ok((0, 0));
    }
    let ts = sys_buf.last().map(|s| s.ts_ms).unwrap_or_else(now_ms);

    let mut aggs = Vec::with_capacity(accs.len());
    for (key, acc) in accs.drain() {
        let row_id = match id_cache.get(&key) {
            Some(id) => *id,
            None => {
                let id = store.upsert_process(&acc.identity, ts)?;
                id_cache.insert(key, id);
                id
            }
        };
        aggs.push(acc.finish(row_id));
    }

    store.write_batch(ts, window_secs, sys_buf, &aggs)?;
    let counts = (1u64, aggs.len() as u64);
    tracing::info!(
        proc_rows = aggs.len(),
        sys_rows = sys_buf.len(),
        window_secs,
        "flushed window"
    );
    sys_buf.clear();
    Ok(counts)
}

fn cmd_db_top(db_path: PathBuf, minutes: u64, limit: u32) -> Result<()> {
    let store = Store::open(&db_path)?;
    let since = now_ms() - (minutes as i64) * 60_000;
    let rows = store.top_processes(since, limit)?;
    if rows.is_empty() {
        println!(
            "No recorded data in the last {minutes} minutes ({}).",
            db_path.display()
        );
        println!("Run `atlas-service record` first.");
        return Ok(());
    }
    println!(
        "Top processes by average CPU over the last {minutes} minutes ({} windows source: {})",
        rows.len(),
        db_path.display()
    );
    println!(
        "{:>7} {:<30} {:>8} {:>9} {:>11} {:>8}",
        "PID", "NAME", "AVG CPU%", "PEAK CPU%", "PEAK WS MB", "WINDOWS"
    );
    for r in rows {
        println!(
            "{:>7} {:<30} {:>8.1} {:>9.1} {:>11.1} {:>8}",
            r.pid,
            truncate(&r.image_name, 30),
            r.cpu_avg_permille / 10.0,
            r.cpu_peak_permille as f64 / 10.0,
            mb(r.working_set_peak),
            r.windows
        );
    }
    Ok(())
}

fn gb(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 30) as f64
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1u64 << 20) as f64
}

fn rate(bps: u64) -> String {
    match bps {
        0 => "-".to_string(),
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}
