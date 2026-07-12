//! Atlas service host (tech-stack.md §4.1).
//!
//! Dev console mode today: `top` / `snapshot` / `record` / `db-top` / `events`
//! subcommands exercise the collection path end-to-end. The `events` command
//! streams live ETW process start/stop (M3). The `serve` command hosts the
//! `AtlasQuery` gRPC contract over a named pipe and `client-snapshot` is its
//! dev client (M4, docs/phases.md). Windows-service mode arrives at M9.

#[cfg(windows)]
mod ipc;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand};

use atlas_collectors::{CadenceController, ProcKey, ProcSample, SampleSet, Sampler, Tick};
use atlas_store::{ProcAggregate, ProcIdentity, SelfSampleRow, Store, SysSampleRow};

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
    /// Stream live process start/stop events via ETW until Ctrl+C.
    ///
    /// Requires an elevated terminal (starting an ETW session needs admin).
    Events,
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
    /// Host the AtlasQuery gRPC contract over a named pipe until Ctrl+C (M4).
    ///
    /// Runs the sampler at 1 s in the background and serves GetCapabilities /
    /// GetSnapshot / StreamSnapshots. Runs unprivileged; the pipe DACL grants
    /// SYSTEM, Administrators, and the current user only.
    Serve {
        /// Override the pipe name discriminator (default: current username).
        #[arg(long)]
        pipe: Option<String>,
    },
    /// Connect to a running `serve` over the pipe and print a snapshot (M4).
    ///
    /// Calls GetCapabilities + GetSnapshot(top_n); with `--watch`, streams one
    /// line per update via StreamSnapshots until Ctrl+C.
    ClientSnapshot {
        /// Override the pipe name discriminator (default: current username).
        #[arg(long)]
        pipe: Option<String>,
        /// Rows to request (0 = all).
        #[arg(long, default_value_t = 10)]
        top_n: u32,
        /// Stream continuous updates instead of a single snapshot.
        #[arg(long)]
        watch: bool,
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
        Cmd::Events => cmd_events(),
        Cmd::DbTop { db, minutes, limit } => {
            cmd_db_top(db.unwrap_or_else(default_db_path), minutes, limit)
        }
        Cmd::Serve { pipe } => cmd_serve(pipe),
        Cmd::ClientSnapshot { pipe, top_n, watch } => cmd_client_snapshot(pipe, top_n, watch),
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

/// Per-process window accumulator. With a variable sampling cadence the ticks
/// in a window no longer cover equal time, so averages are weighted by each
/// tick's wall-clock duration: avg = Σ(value × dt) / Σ(dt). Maxima are still
/// plain maxima; "last" fields keep the most recent value.
struct AggAcc {
    identity: ProcIdentity,
    /// Σ(dt) in seconds across the ticks folded in — the weighting basis.
    weight_s: f64,
    cpu_weighted: f64,
    cpu_max: u32,
    ws_max: u64,
    priv_max: u64,
    read_bps_weighted: f64,
    write_bps_weighted: f64,
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
            weight_s: 0.0,
            cpu_weighted: 0.0,
            cpu_max: 0,
            ws_max: 0,
            priv_max: 0,
            read_bps_weighted: 0.0,
            write_bps_weighted: 0.0,
            handles_last: 0,
            threads_last: 0,
        }
    }

    fn update(&mut self, p: &ProcSample, dt_s: f64) {
        self.weight_s += dt_s;
        self.cpu_weighted += p.cpu_permille as f64 * dt_s;
        self.cpu_max = self.cpu_max.max(p.cpu_permille);
        self.ws_max = self.ws_max.max(p.working_set);
        self.priv_max = self.priv_max.max(p.private_bytes);
        self.read_bps_weighted += p.read_bps as f64 * dt_s;
        self.write_bps_weighted += p.write_bps as f64 * dt_s;
        self.handles_last = p.handle_count;
        self.threads_last = p.thread_count;
    }

    fn finish(&self, proc_row_id: i64) -> ProcAggregate {
        // Guard against a zero-weight window (all dt collapsed to ~0).
        let w = if self.weight_s > 0.0 {
            self.weight_s
        } else {
            1.0
        };
        ProcAggregate {
            proc_row_id,
            cpu_avg_permille: (self.cpu_weighted / w).round() as u32,
            cpu_max_permille: self.cpu_max,
            working_set_max: self.ws_max,
            private_bytes_max: self.priv_max,
            read_bps_avg: (self.read_bps_weighted / w).round() as u64,
            write_bps_avg: (self.write_bps_weighted / w).round() as u64,
            handles_last: self.handles_last,
            threads_last: self.threads_last,
        }
    }
}

/// Time-weighted accumulator for Atlas's own overhead over a flush window
/// (PRD §12.2). CPU is weighted by tick duration like [`AggAcc`]; the tick
/// duration stats time the `sampler.sample()` call itself.
struct SelfAcc {
    weight_s: f64,
    cpu_weighted: f64,
    working_set_last: u64,
    tick_us_sum: u64,
    tick_us_max: u64,
    ticks: u32,
}

impl SelfAcc {
    fn new() -> Self {
        Self {
            weight_s: 0.0,
            cpu_weighted: 0.0,
            working_set_last: 0,
            tick_us_sum: 0,
            tick_us_max: 0,
            ticks: 0,
        }
    }

    /// Fold one tick: `own` is this process's sample (may be absent for one
    /// tick if newly seen), `dt_s` the wall-clock gap, `tick_us` the measured
    /// `sample()` duration.
    fn update(&mut self, own: Option<&ProcSample>, dt_s: f64, tick_us: u64) {
        if let Some(p) = own {
            self.weight_s += dt_s;
            self.cpu_weighted += p.cpu_permille as f64 * dt_s;
            self.working_set_last = p.working_set;
        }
        self.tick_us_sum += tick_us;
        self.tick_us_max = self.tick_us_max.max(tick_us);
        self.ticks += 1;
    }

    fn finish(&self, ts_ms: i64) -> SelfSampleRow {
        let w = if self.weight_s > 0.0 {
            self.weight_s
        } else {
            1.0
        };
        let ticks = self.ticks.max(1);
        SelfSampleRow {
            ts_ms,
            cpu_permille: (self.cpu_weighted / w).round() as u32,
            working_set: self.working_set_last,
            tick_duration_us_avg: self.tick_us_sum / ticks as u64,
            tick_duration_us_max: self.tick_us_max,
            ticks: self.ticks,
        }
    }
}

/// A complete flush window handed to the writer thread. It carries everything
/// the writer needs to persist a window without touching the collection loop's
/// state: the aggregation timestamp, window length, system rows, per-process
/// aggregates *with their identity* (the writer owns the id cache and does the
/// upsert/mark-exited bookkeeping), exited keys, the self-metrics row, and the
/// count of windows dropped since the last successful send (PRD §11.3).
struct FlushBatch {
    agg_ts_ms: i64,
    window_secs: u32,
    sys: Vec<SysSampleRow>,
    procs: Vec<(ProcIdentity, ProcAggregate)>,
    exited: Vec<ProcKey>,
    self_row: SelfSampleRow,
    dropped_before: u64,
}

const RETENTION_HOURS: i64 = 72;

fn cmd_record(
    db_path: PathBuf,
    interval: f64,
    flush_secs: u64,
    duration: Option<u64>,
) -> Result<()> {
    let stop = install_ctrlc();
    // The store lives entirely on the writer thread; the sampling loop never
    // touches SQLite (M2). A small bound (4) gives the writer slack without
    // letting a stall balloon memory: past that we drop batches and record a
    // gap rather than block collection.
    let (tx, rx) = sync_channel::<FlushBatch>(4);
    let writer_db = db_path.clone();
    let writer = std::thread::Builder::new()
        .name("atlas-writer".into())
        .spawn(move || writer_thread(writer_db, rx))?;

    let mut sampler = Sampler::new()?;
    let own_pid = std::process::id();
    let mut cadence = CadenceController::new();
    tracing::info!(db = %db_path.display(), interval, flush_secs, "recording started (Ctrl+C to stop)");

    let started = Instant::now();
    let flush_every = Duration::from_secs(flush_secs.max(2));
    let mut last_flush = Instant::now();
    // The configured `interval` is now the active-tier floor; the cadence
    // controller widens it toward 5 s / 15 s during sustained quiet.
    let mut next_sleep = Duration::from_secs_f64(interval.max(0.25));

    let mut accs: HashMap<ProcKey, AggAcc> = HashMap::new();
    let mut sys_buf: Vec<SysSampleRow> = Vec::new();
    let mut self_acc = SelfAcc::new();
    let mut prev_tick = Instant::now();
    // Windows dropped because the writer stalled, carried into the next batch
    // that lands (PRD §11.3 — degradation is observable, never silent).
    let mut dropped_pending = 0u64;
    let mut sent_batches = 0u64;

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if let Some(secs) = duration {
            if started.elapsed() >= Duration::from_secs(secs) {
                break;
            }
        }
        std::thread::sleep(next_sleep);

        // Time the sample() call itself — this is the dominant cost of a tick
        // and what the self-metrics report as tick duration.
        let t0 = Instant::now();
        let set = sampler.sample()?;
        let tick_us = t0.elapsed().as_micros() as u64;
        let dt_s = prev_tick.elapsed().as_secs_f64().max(1e-3);
        prev_tick = Instant::now();

        // Feed the cadence controller and pick the next sleep. The floor keeps
        // the active tier no faster than the user asked for.
        let max_proc_cpu = set
            .processes
            .iter()
            .map(|p| p.cpu_permille)
            .max()
            .unwrap_or(0);
        let chosen = cadence.next_interval(Tick {
            sys_cpu_permille: set.system.cpu_permille,
            started: set.started.len() as u32,
            exited: set.exited.len() as u32,
            max_proc_cpu_permille: max_proc_cpu,
            elapsed: Duration::from_secs_f64(dt_s),
        });
        next_sleep = chosen.max(Duration::from_secs_f64(interval.max(0.25)));

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
        let own = set.processes.iter().find(|p| p.key.pid == own_pid);
        self_acc.update(own, dt_s, tick_us);
        for p in &set.processes {
            accs.entry(p.key)
                .or_insert_with(|| AggAcc::new(p))
                .update(p, dt_s);
        }

        if last_flush.elapsed() >= flush_every {
            let window_secs = last_flush.elapsed().as_secs().max(1) as u32;
            if let Some(batch) = build_batch(
                &mut accs,
                &mut sys_buf,
                &set.exited,
                &self_acc,
                window_secs,
                dropped_pending,
            ) {
                match tx.try_send(batch) {
                    Ok(()) => {
                        sent_batches += 1;
                        dropped_pending = 0;
                    }
                    Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                        // Writer is behind (or gone): drop this window, count
                        // it, and let the next successful batch report the gap.
                        dropped_pending += 1;
                        tracing::warn!(dropped_pending, "writer stalled; dropped flush window");
                    }
                }
            }
            self_acc = SelfAcc::new();
            last_flush = Instant::now();
        }
    }

    // Final partial window before shutdown.
    let window_secs = last_flush.elapsed().as_secs().max(1) as u32;
    if let Some(batch) = build_batch(
        &mut accs,
        &mut sys_buf,
        &[],
        &self_acc,
        window_secs,
        dropped_pending,
    ) {
        if tx.try_send(batch).is_ok() {
            sent_batches += 1;
        } else {
            tracing::warn!("final flush window dropped (writer stalled)");
        }
    }

    // Clean shutdown: drop the sender so the writer drains, sweeps retention,
    // and exits; join it before we return (M2).
    drop(tx);
    let pruned = writer
        .join()
        .map_err(|_| anyhow::anyhow!("writer thread panicked"))??;
    tracing::info!(
        sent_batches,
        pruned_proc = pruned.0,
        pruned_sys = pruned.1,
        "recording stopped"
    );
    Ok(())
}

/// Drains the current accumulators into a self-contained [`FlushBatch`]. The
/// per-process aggregates carry a placeholder row id (-1); the writer thread
/// resolves the real id via its own cache before persisting. Returns `None`
/// when there is nothing to write.
fn build_batch(
    accs: &mut HashMap<ProcKey, AggAcc>,
    sys_buf: &mut Vec<SysSampleRow>,
    exited: &[ProcKey],
    self_acc: &SelfAcc,
    window_secs: u32,
    dropped_before: u64,
) -> Option<FlushBatch> {
    if accs.is_empty() && sys_buf.is_empty() {
        return None;
    }
    let ts = sys_buf.last().map(|s| s.ts_ms).unwrap_or_else(now_ms);
    let procs = accs
        .drain()
        .map(|(_, acc)| {
            let agg = acc.finish(-1);
            (acc.identity, agg)
        })
        .collect();
    Some(FlushBatch {
        agg_ts_ms: ts,
        window_secs,
        sys: std::mem::take(sys_buf),
        procs,
        exited: exited.to_vec(),
        self_row: self_acc.finish(ts),
        dropped_before,
    })
}

/// Dedicated writer thread: owns the `Store` and the process id cache, applies
/// each batch in one transaction, records any dropped-window gaps, and sweeps
/// 72 h retention on shutdown. Returns (pruned_proc, pruned_sys) rows.
fn writer_thread(
    db_path: PathBuf,
    rx: std::sync::mpsc::Receiver<FlushBatch>,
) -> Result<(usize, usize)> {
    let mut store = Store::open(&db_path)?;
    let mut id_cache: HashMap<ProcKey, i64> = HashMap::new();

    for batch in rx {
        // Any windows the sampler dropped since the last landed batch are
        // recorded as a gap so charts can render missing data honestly.
        if batch.dropped_before > 0 {
            store.record_gap(batch.agg_ts_ms, batch.dropped_before, "writer backpressure")?;
        }

        // Resolve identities → row ids (this is the upsert bookkeeping moved
        // off the sampling loop) and stamp them onto the aggregates.
        let mut aggs = Vec::with_capacity(batch.procs.len());
        for (identity, mut agg) in batch.procs {
            let key = ProcKey {
                pid: identity.pid,
                create_time_100ns: identity.create_time_100ns,
            };
            let row_id = match id_cache.get(&key) {
                Some(id) => *id,
                None => {
                    let id = store.upsert_process(&identity, batch.agg_ts_ms)?;
                    id_cache.insert(key, id);
                    id
                }
            };
            agg.proc_row_id = row_id;
            aggs.push(agg);
        }
        for key in &batch.exited {
            if let Some(row_id) = id_cache.remove(key) {
                store.mark_exited(row_id, batch.agg_ts_ms)?;
            }
        }

        store.write_batch(batch.agg_ts_ms, batch.window_secs, &batch.sys, &aggs)?;
        store.write_self_sample(&batch.self_row)?;
        tracing::debug!(
            cpu_permille = batch.self_row.cpu_permille,
            working_set = batch.self_row.working_set,
            tick_us_avg = batch.self_row.tick_duration_us_avg,
            tick_us_max = batch.self_row.tick_duration_us_max,
            ticks = batch.self_row.ticks,
            "self metrics"
        );
        tracing::info!(
            proc_rows = aggs.len(),
            sys_rows = batch.sys.len(),
            window_secs = batch.window_secs,
            "flushed window"
        );
    }

    let cutoff = now_ms() - RETENTION_HOURS * 3_600_000;
    let pruned = store.apply_retention(cutoff)?;
    Ok(pruned)
}

/// Exit code returned when the ETW session cannot start because the process is
/// not elevated — lets callers/scripts distinguish this from other failures.
const EXIT_ELEVATION_REQUIRED: i32 = 2;

#[cfg(windows)]
fn cmd_events() -> Result<()> {
    use atlas_collectors::{EventError, ProcessEventWatcher};

    let stop = install_ctrlc();

    let (watcher, rx) = match ProcessEventWatcher::start() {
        Ok(pair) => pair,
        Err(EventError::ElevationRequired) => {
            eprintln!(
                "Starting an ETW session requires administrator rights. \
                 Rerun this command from an elevated (Run as administrator) terminal."
            );
            std::process::exit(EXIT_ELEVATION_REQUIRED);
        }
        Err(e) => return Err(anyhow::anyhow!(e.to_string())),
    };

    tracing::info!(
        session = watcher.session_name(),
        "ETW process events started (Ctrl+C to stop)"
    );
    println!(
        "Streaming process events on {} (Ctrl+C to stop)",
        watcher.session_name()
    );

    // Drain the channel with a short timeout so Ctrl+C is observed promptly even
    // when no events arrive.
    while !stop.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ev) => println!("{}", format_event(&ev)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let dropped = watcher.dropped_count();
    watcher.stop().map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if dropped > 0 {
        tracing::warn!(dropped, "some events were dropped (channel full)");
    }
    tracing::info!("ETW process events stopped");
    Ok(())
}

#[cfg(not(windows))]
fn cmd_events() -> Result<()> {
    anyhow::bail!("the `events` command requires Windows ETW");
}

/// Render one event as a line, matching the M3 spec format:
/// `[21:04:11.123] START pid=1234 parent=5678 session=1 notepad.exe`
/// `[21:04:15.001] STOP  pid=1234 exit=0`
#[cfg(windows)]
fn format_event(ev: &atlas_collectors::ProcessEvent) -> String {
    use atlas_collectors::ProcessEventKind;
    let ts = format_ts(ev.ts_ms);
    match &ev.kind {
        ProcessEventKind::Started {
            parent_pid,
            session_id,
            image_name,
        } => format!(
            "[{ts}] START pid={} parent={} session={} {}",
            ev.pid, parent_pid, session_id, image_name
        ),
        ProcessEventKind::Stopped { exit_status } => {
            format!("[{ts}] STOP  pid={} exit={}", ev.pid, exit_status)
        }
    }
}

/// Format a Unix-epoch-ms timestamp as `HH:MM:SS.mmm` wall-clock time of day.
/// UTC-based (no timezone crate in the dev tool); good enough to correlate the
/// stream by eye.
fn format_ts(ts_ms: i64) -> String {
    let ms_of_day = ts_ms.rem_euclid(86_400_000);
    let ms = ms_of_day % 1000;
    let secs = ms_of_day / 1000;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

/// Resolves the pipe name from an optional discriminator override, falling
/// back to the default (current-user-scoped) name.
#[cfg(windows)]
fn resolve_pipe_name(pipe: Option<String>) -> String {
    match pipe {
        Some(who) => atlas_ipc::pipe_name(&who),
        None => atlas_ipc::default_pipe_name(),
    }
}

/// `serve`: host AtlasQuery over the named pipe until Ctrl+C.
#[cfg(windows)]
fn cmd_serve(pipe: Option<String>) -> Result<()> {
    use atlas_ipc::AtlasQueryServer;

    let name = resolve_pipe_name(pipe);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        let service = ipc::QueryService::start()?;
        let handle = std::sync::Arc::new(service);
        let router = tonic::transport::Server::builder()
            .add_service(AtlasQueryServer::from_arc(handle.clone()));

        tracing::info!(pipe = %name, "AtlasQuery serving (Ctrl+C to stop)");
        println!("Serving AtlasQuery on {name} (Ctrl+C to stop)");

        let shutdown = async {
            let _ = tokio::signal::ctrl_c().await;
        };
        let result = atlas_ipc::serve(&name, router, shutdown).await;
        handle.shutdown();
        result
    })?;

    tracing::info!("AtlasQuery server stopped");
    Ok(())
}

/// `client-snapshot`: connect to a running `serve` and print capabilities +
/// a snapshot (or stream with `--watch`).
#[cfg(windows)]
fn cmd_client_snapshot(pipe: Option<String>, top_n: u32, watch: bool) -> Result<()> {
    use atlas_ipc::{AtlasQueryClient, CapabilitiesRequest, SnapshotRequest};

    let name = resolve_pipe_name(pipe);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    rt.block_on(async move {
        let channel = atlas_ipc::connect(&name)
            .await
            .map_err(|e| anyhow::anyhow!("connect to {name}: {e}"))?;
        let mut client = AtlasQueryClient::new(channel);

        let caps = client
            .get_capabilities(CapabilitiesRequest {})
            .await?
            .into_inner();
        println!(
            "Capabilities: service_version={} flags=[{}]",
            caps.service_version,
            caps.capability_flags.join(", ")
        );

        if watch {
            let mut stream = client
                .stream_snapshots(SnapshotRequest { top_n })
                .await?
                .into_inner();
            println!("Streaming snapshots (Ctrl+C to stop)");
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    item = stream.message() => match item? {
                        Some(reply) => println!("{}", format_snapshot_line(&reply)),
                        None => break,
                    },
                }
            }
        } else {
            let reply = client
                .get_snapshot(SnapshotRequest { top_n })
                .await?
                .into_inner();
            print_snapshot(&reply);
        }
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(())
}

/// One-line summary of a snapshot for `--watch`.
#[cfg(windows)]
fn format_snapshot_line(reply: &atlas_ipc::SnapshotReply) -> String {
    let sys = reply.system.as_ref();
    let cpu = sys.map(|s| s.cpu_permille as f64 / 10.0).unwrap_or(0.0);
    let top = reply
        .processes
        .first()
        .map(|p| format!("{} {:.1}%", p.image_name, p.cpu_permille as f64 / 10.0))
        .unwrap_or_else(|| "-".to_string());
    format!(
        "CPU {cpu:>5.1}%  procs {:>4}  top: {top}",
        reply.processes.len()
    )
}

/// Full snapshot dump for the one-shot client (the M4 dev proof).
#[cfg(windows)]
fn print_snapshot(reply: &atlas_ipc::SnapshotReply) {
    if let Some(s) = &reply.system {
        println!(
            "System: CPU {:.1}%  Memory {:.1}/{:.1} GB  Commit {:.1}/{:.1} GB  {} processes, {} threads, {} handles",
            s.cpu_permille as f64 / 10.0,
            gb(s.mem_used),
            gb(s.mem_total),
            gb(s.commit_used),
            gb(s.commit_limit),
            s.process_count,
            s.thread_count,
            s.handle_count
        );
    }
    println!(
        "{:>7} {:<30} {:>6} {:>9} {:>9} {:>5} {:>7}",
        "PID", "NAME", "CPU%", "WS MB", "PRIV MB", "THR", "HANDLE"
    );
    for p in &reply.processes {
        println!(
            "{:>7} {:<30} {:>6.1} {:>9.1} {:>9.1} {:>5} {:>7}",
            p.pid,
            truncate(&p.image_name, 30),
            p.cpu_permille as f64 / 10.0,
            mb(p.working_set),
            mb(p.private_bytes),
            p.thread_count,
            p.handle_count
        );
    }
}

#[cfg(not(windows))]
fn cmd_serve(_pipe: Option<String>) -> Result<()> {
    anyhow::bail!("the `serve` command requires Windows named pipes");
}

#[cfg(not(windows))]
fn cmd_client_snapshot(_pipe: Option<String>, _top_n: u32, _watch: bool) -> Result<()> {
    anyhow::bail!("the `client-snapshot` command requires Windows named pipes");
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
        print_self_summary(&store)?;
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
    print_self_summary(&store)?;
    Ok(())
}

/// Prints Atlas's own overhead from the latest self_sample row so it is
/// verifiable without a SQLite client (PRD §12.2).
fn print_self_summary(store: &Store) -> Result<()> {
    match store.latest_self_sample()? {
        Some(s) => println!(
            "Atlas overhead: {:.1}% CPU avg, {:.1} MB WS, tick avg {:.1} ms (max {:.1} ms over {} ticks)",
            s.cpu_permille as f64 / 10.0,
            mb(s.working_set),
            s.tick_duration_us_avg as f64 / 1000.0,
            s.tick_duration_us_max as f64 / 1000.0,
            s.ticks
        ),
        None => println!("Atlas overhead: no self-metrics recorded yet."),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_ts_renders_time_of_day() {
        // 21:04:11.123 into the UTC day.
        let ms = (21 * 3600 + 4 * 60 + 11) * 1000 + 123;
        assert_eq!(format_ts(ms), "21:04:11.123");
    }

    #[test]
    fn format_ts_pads_and_wraps() {
        assert_eq!(format_ts(0), "00:00:00.000");
        // One day plus 1 ms wraps back to the start of the day.
        assert_eq!(format_ts(86_400_000 + 1), "00:00:00.001");
    }

    #[cfg(windows)]
    #[test]
    fn format_event_start_and_stop() {
        use atlas_collectors::{ProcessEvent, ProcessEventKind};
        let ts = (21 * 3600 + 4 * 60 + 11) * 1000 + 123;
        let start = ProcessEvent {
            ts_ms: ts,
            pid: 1234,
            kind: ProcessEventKind::Started {
                parent_pid: 5678,
                session_id: 1,
                image_name: "notepad.exe".into(),
            },
        };
        assert_eq!(
            format_event(&start),
            "[21:04:11.123] START pid=1234 parent=5678 session=1 notepad.exe"
        );

        let stop = ProcessEvent {
            ts_ms: ts + 3878,
            pid: 1234,
            kind: ProcessEventKind::Stopped { exit_status: 0 },
        };
        assert_eq!(format_event(&stop), "[21:04:15.001] STOP  pid=1234 exit=0");
    }
}
