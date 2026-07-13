//! Atlas service host (tech-stack.md §4.1).
//!
//! Dev console mode today: `top` / `snapshot` / `record` / `db-top` / `events`
//! subcommands exercise the collection path end-to-end. The `events` command
//! streams live ETW process start/stop (M3). The `serve` command hosts the
//! `AtlasQuery` gRPC contract over a named pipe and `client-snapshot` is its
//! dev client (M4, docs/phases.md). Windows-service mode arrives at M9.

#[cfg(windows)]
mod broker;
mod detectors;
mod diagnostics;
#[cfg(windows)]
mod ipc;
mod report;
mod service_ctl;
mod soak;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::{Parser, Subcommand};

use atlas_collectors::{CadenceController, ProcKey, ProcSample, SampleSet, Sampler, Tick};
use atlas_store::{
    ProcEventRow, ProcIdentity, SelfSampleRow, Store, PROC_EVENT_START, PROC_EVENT_STOP,
};
use atlas_tsdb::{HeadBlocks, Metric, SeriesKey, SYSTEM_SCOPE};

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
    Events {
        /// Also stream image-load events (higher volume; opt-in).
        #[arg(long)]
        images: bool,
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
    /// Host the AtlasQuery gRPC contract over a named pipe until Ctrl+C (M4).
    ///
    /// Runs the sampler at 1 s in the background and serves GetCapabilities /
    /// GetSnapshot / StreamSnapshots. Runs unprivileged; the pipe DACL grants
    /// SYSTEM, Administrators, and the current user only.
    Serve {
        /// Override the pipe name discriminator (default: current username).
        #[arg(long)]
        pipe: Option<String>,
        /// Store path for history queries + audit (default: dev atlas.db). This
        /// is the same file `record` writes; WAL keeps the two connections
        /// coexisting.
        #[arg(long)]
        db: Option<PathBuf>,
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
    /// Attach to a running `serve`'s shared-memory live ring and print it (M4).
    ///
    /// Lock-free read path (seqlock) — the future emergency-UI fast path. Uses
    /// the same discriminator as `serve --pipe` to rendezvous. With `--watch`,
    /// repaints ~1 Hz until Ctrl+C.
    RingRead {
        /// Ring discriminator; must match the server's `serve --pipe` token
        /// (default: current username).
        #[arg(long)]
        pipe: Option<String>,
        /// Rows to display.
        #[arg(long, default_value_t = 15)]
        limit: usize,
        /// Repaint continuously (~1 Hz) instead of a single read.
        #[arg(long)]
        watch: bool,
    },
    /// Print decimated history buckets for a metric over a look-back window (M6).
    ///
    /// Exercises the same `query_range` the AtlasQuery RPC serves, straight
    /// against the store — no `serve` needed.
    History {
        /// Database path (default: %LOCALAPPDATA%\SystemAtlas\dev\atlas.db).
        #[arg(long)]
        db: Option<PathBuf>,
        /// Metric to query: sys-cpu | sys-mem | sys-commit | sys-procs |
        /// cpu | ws | priv | read | write (the per-process ones need --scope).
        #[arg(long, default_value = "sys-cpu")]
        metric: String,
        /// Per-process scope (process_instance row id); ignored for sys-* metrics.
        #[arg(long, default_value_t = 0)]
        scope: i64,
        /// Look-back window in minutes.
        #[arg(long, default_value_t = 10)]
        minutes: u64,
        /// Decimation target (max buckets).
        #[arg(long, default_value_t = 60)]
        buckets: u32,
    },
    /// Full-text/substring search over processes, events, and bookmarks (M6).
    Search {
        #[arg(long)]
        db: Option<PathBuf>,
        /// The query string (name / pid / bookmark label).
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Manage incident bookmarks (M6): `bookmark add "<label>"` / `bookmark list`.
    Bookmark {
        #[arg(long)]
        db: Option<PathBuf>,
        #[command(subcommand)]
        cmd: BookmarkCmd,
    },
    /// Prepare (and optionally execute) a safe process action (M6 broker).
    ///
    /// DEFAULT IS DRY-RUN: without `--yes` this runs Prepare only and prints the
    /// risk picture + verdict; it never touches the target. With `--yes` it runs
    /// Prepare then Execute against the SAME in-process broker. Test suspend/
    /// resume/close/terminate on a throwaway process you spawned — never a system
    /// process (the protected-critical list denies those anyway).
    Action {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Target process id.
        #[arg(long)]
        pid: u32,
        /// Action verb: suspend | resume | close | terminate.
        #[arg(long = "do")]
        action: String,
        /// Actually execute after preparing (default: dry-run / prepare only).
        #[arg(long)]
        yes: bool,
    },
    /// Print the most recent safe-action audit rows (M6 verification helper).
    Audit {
        #[arg(long)]
        db: Option<PathBuf>,
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
    /// Print the current privacy-capability usage from the ConsentStore (M7).
    ///
    /// Point-in-time read of camera/mic/location usage per app (PRD §9.10) —
    /// the same data `ListPrivacyUsage` serves. Unprivileged.
    Privacy,
    /// Print the startup inventory grouped by source (M7).
    ///
    /// Run keys, Startup folders, and StartupApproved state (PRD §9.8.1) — the
    /// same data `ListStartup` serves. Unprivileged.
    Startup,
    /// Print the Win32 services inventory as a table (M7).
    ///
    /// SCM enumeration + config (PRD §9.9.1) — the same data `ListServices`
    /// serves. Unprivileged.
    Services {
        /// Case-insensitive substring over name/display_name (empty = all).
        #[arg(long)]
        filter: Option<String>,
    },
    /// Measure Atlas's own collection overhead against the PRD budgets (M3).
    ///
    /// Runs the real record pipeline against a TEMP database for `--duration`
    /// seconds, then reports own CPU/working-set, sampler tick timing, disk
    /// write volume, and ETW live/degraded status with PASS/FAIL vs budget.
    /// The temp database is deleted afterwards. Always exits 0 (informational;
    /// M9 turns it into a CI gate).
    Overhead {
        /// Measurement duration in seconds.
        #[arg(long, default_value_t = 30)]
        duration: u64,
        /// Sampling interval floor in seconds (matches `record`).
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
        /// Aggregation/flush window in seconds (matches `record`).
        #[arg(long, default_value_t = 15)]
        flush_secs: u64,
        /// Emit a single machine-readable JSON line instead of the human report
        /// (the CI perf gate parses this; field names are stable — M9).
        #[arg(long)]
        json: bool,
    },
    /// List detected incidents over a look-back window (M8).
    ///
    /// Refreshes detection over the window (idempotent) then lists incidents.
    Incidents {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Look-back window in minutes.
        #[arg(long, default_value_t = 60)]
        minutes: u64,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Print the structured diagnosis for an incident or an ad-hoc range (M8).
    ///
    /// Evidence-based, no LLM: peak metrics, ranked contributing factors with
    /// PRD-ladder confidence, and a templated recommendation (PRD §9.15).
    Diagnose {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Diagnose a detected incident by id (from `incidents`).
        #[arg(long)]
        incident: Option<i64>,
        /// Ad-hoc: diagnose the last N minutes instead of an incident.
        #[arg(long)]
        minutes: Option<u64>,
    },
    /// Render an incident diagnosis report (M8): text | json | csv | html.
    ///
    /// Applies a redaction pass (user/computer names, paths, command lines)
    /// before formatting so every format is redacted identically (PRD §9.18).
    Report {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Incident id to report on (from `incidents`).
        #[arg(long)]
        incident: Option<i64>,
        /// Ad-hoc range in minutes instead of an incident.
        #[arg(long)]
        minutes: Option<u64>,
        /// Output format: text | json | csv | html.
        #[arg(long, default_value = "text")]
        format: String,
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Replace the current user name with <USER>.
        #[arg(long)]
        redact_users: bool,
        /// Replace the computer name with <HOST>.
        #[arg(long)]
        redact_computer: bool,
        /// Replace file paths with <PATH>.
        #[arg(long)]
        redact_paths: bool,
        /// Replace command-line arguments with <CMD-ARGS>.
        #[arg(long)]
        redact_command_lines: bool,
    },
    /// Manage the Windows service host (M9): install | uninstall | run | status.
    ///
    /// `install`/`uninstall` need an elevated terminal (they touch the SCM); an
    /// unprivileged run prints a clear "run elevated" message and exits with a
    /// distinct code, exactly like the ETW path. `run` is the SCM entry point and
    /// is meant to be launched by the Service Control Manager, not by hand.
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
    },
    /// Leak-detection soak: run the record pipeline for N minutes, fit an RSS
    /// slope + peak handle growth on its own metrics, print PASS/FAIL (M9).
    ///
    /// Designed to run short in CI (a few minutes) and long (72 h) manually. The
    /// verdict fails if extrapolated RSS growth exceeds the slope threshold or
    /// handle growth exceeds its threshold (PRD §12.2 — the tool watches itself).
    Soak {
        /// Duration in minutes.
        #[arg(long, default_value_t = 3)]
        minutes: u64,
        /// Self-sampling period in seconds (how often own RSS/handles are read).
        #[arg(long, default_value_t = 10)]
        sample_secs: u64,
        /// Sampling interval floor for the underlying record pipeline.
        #[arg(long, default_value_t = 1.0)]
        interval: f64,
        /// Flush window for the underlying record pipeline.
        #[arg(long, default_value_t = 15)]
        flush_secs: u64,
        /// RSS-slope failure threshold, MB/hour (extrapolated).
        #[arg(long, default_value_t = soak::DEFAULT_SLOPE_THRESHOLD_MB_PER_HOUR)]
        slope_threshold: f64,
        /// Peak handle-growth failure threshold.
        #[arg(long, default_value_t = soak::DEFAULT_HANDLE_GROWTH_THRESHOLD)]
        handle_threshold: i64,
        /// Warmup window (seconds) excluded from the slope fit, so the one-time
        /// startup RSS ramp is not mistaken for a leak.
        #[arg(long, default_value_t = soak::DEFAULT_WARMUP_SECS)]
        warmup_secs: f64,
    },
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Register the service (auto-start, runs `service run`) with crash-restart
    /// failure actions. Needs elevation.
    Install,
    /// Stop and delete the service. Needs elevation.
    Uninstall,
    /// The SCM entry point — connects to the Service Control Manager and runs the
    /// collection + serve loop. Launched by the SCM, not by hand.
    Run,
    /// Query and print the service's current state.
    Status,
}

#[derive(Subcommand)]
enum BookmarkCmd {
    /// Add a bookmark at the current time (or `--at <ms>`).
    Add {
        /// The label text.
        label: String,
        /// Unix-epoch ms to bookmark (default: now).
        #[arg(long)]
        at: Option<i64>,
    },
    /// List bookmarks, optionally within a [--from, --to] ms window.
    List {
        #[arg(long)]
        from: Option<i64>,
        #[arg(long)]
        to: Option<i64>,
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
        Cmd::Events { images } => cmd_events(images),
        Cmd::DbTop { db, minutes, limit } => {
            cmd_db_top(db.unwrap_or_else(default_db_path), minutes, limit)
        }
        Cmd::History {
            db,
            metric,
            scope,
            minutes,
            buckets,
        } => cmd_history(
            db.unwrap_or_else(default_db_path),
            &metric,
            scope,
            minutes,
            buckets,
        ),
        Cmd::Search { db, query, limit } => {
            cmd_search(db.unwrap_or_else(default_db_path), &query, limit)
        }
        Cmd::Bookmark { db, cmd } => cmd_bookmark(db.unwrap_or_else(default_db_path), cmd),
        Cmd::Action {
            db,
            pid,
            action,
            yes,
        } => cmd_action(db.unwrap_or_else(default_db_path), pid, &action, yes),
        Cmd::Audit { db, limit } => cmd_audit(db.unwrap_or_else(default_db_path), limit),
        Cmd::Privacy => cmd_privacy(),
        Cmd::Startup => cmd_startup(),
        Cmd::Services { filter } => cmd_services(filter.unwrap_or_default()),
        Cmd::Serve { pipe, db } => cmd_serve(pipe, db.unwrap_or_else(default_db_path)),
        Cmd::ClientSnapshot { pipe, top_n, watch } => cmd_client_snapshot(pipe, top_n, watch),
        Cmd::RingRead { pipe, limit, watch } => cmd_ring_read(pipe, limit, watch),
        Cmd::Overhead {
            duration,
            interval,
            flush_secs,
            json,
        } => cmd_overhead(duration, interval, flush_secs, json),
        Cmd::Service { cmd } => cmd_service(cmd),
        Cmd::Soak {
            minutes,
            sample_secs,
            interval,
            flush_secs,
            slope_threshold,
            handle_threshold,
            warmup_secs,
        } => cmd_soak(
            minutes,
            sample_secs,
            interval,
            flush_secs,
            slope_threshold,
            handle_threshold,
            warmup_secs,
        ),
        Cmd::Incidents { db, minutes, limit } => {
            cmd_incidents(db.unwrap_or_else(default_db_path), minutes, limit)
        }
        Cmd::Diagnose {
            db,
            incident,
            minutes,
        } => cmd_diagnose(db.unwrap_or_else(default_db_path), incident, minutes),
        Cmd::Report {
            db,
            incident,
            minutes,
            format,
            out,
            redact_users,
            redact_computer,
            redact_paths,
            redact_command_lines,
        } => cmd_report(
            db.unwrap_or_else(default_db_path),
            incident,
            minutes,
            &format,
            out,
            atlas_ipc::RedactionOptions {
                redact_user_names: redact_users,
                redact_computer_name: redact_computer,
                redact_paths,
                redact_command_lines,
            },
        ),
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

/// The five per-process metric values captured for one tick. These are the raw
/// samples the writer appends into the Gorilla head blocks (M-TSDB) — no more
/// per-window averaging on the sampling loop; the block store keeps every tick.
#[derive(Clone, Copy)]
struct ProcMetrics {
    cpu_permille: u32,
    working_set: u64,
    private_bytes: u64,
    read_bps: u64,
    write_bps: u64,
}

impl ProcMetrics {
    fn from_sample(p: &ProcSample) -> Self {
        Self {
            cpu_permille: p.cpu_permille,
            working_set: p.working_set,
            private_bytes: p.private_bytes,
            read_bps: p.read_bps,
            write_bps: p.write_bps,
        }
    }
}

/// System gauges captured for one tick (the six `Sys*` series). `mem_total` is
/// not itself a recorded series (it is effectively constant for a machine) but
/// is carried here so the incident detectors can turn recorded `SysMemUsed`
/// bytes into a percent of total for the memory-pressure threshold (M8).
#[derive(Clone, Copy)]
struct SysMetrics {
    cpu_permille: u32,
    mem_used: u64,
    mem_total: u64,
    commit_used: u64,
    process_count: u32,
    thread_count: u32,
    handle_count: u32,
}

/// One tick's worth of raw samples handed to the writer: the timestamp, the
/// system gauges, and every process seen with its identity (so the writer can
/// resolve the scope/row-id) and its five metric values.
struct TickSamples {
    ts_ms: i64,
    sys: SysMetrics,
    procs: Vec<(ProcIdentity, ProcMetrics)>,
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
/// the writer needs without touching the collection loop's state: the raw
/// per-tick samples (the writer resolves identities to scopes and appends them
/// into its Gorilla head blocks — M-TSDB), the exited keys, the self-metrics
/// row, and the count of windows dropped since the last successful send
/// (PRD §11.3).
struct FlushBatch {
    /// Timestamp used for gap/self rows when there are no ticks (falls back to
    /// wall clock). Individual samples carry their own tick timestamps.
    agg_ts_ms: i64,
    /// Raw per-tick samples accumulated over the flush window.
    ticks: Vec<TickSamples>,
    exited: Vec<ProcKey>,
    self_row: SelfSampleRow,
    dropped_before: u64,
    /// Raw ETW process lifecycle events drained during this window (empty in
    /// degraded mode). Persisted to `proc_event` in the writer's transaction.
    proc_events: Vec<ProcEventRow>,
    /// Exact exits from ETW Stop events. The writer stamps these onto the
    /// matching live instance by pid, superseding the coarser snapshot-diff
    /// `exited` marking. Empty in degraded mode.
    exit_stamps: Vec<ExitStamp>,
}

const RETENTION_HOURS: i64 = 72;

/// Rolling window the per-flush incident detection pass scans (M8). Short
/// incidents only become visible once their sample blocks seal (point/age caps),
/// so a final full-span pass also runs at writer shutdown; detection is
/// idempotent (`upsert_incident` keys by `(kind, start_ms)`) so overlapping
/// passes never duplicate an incident.
const DETECT_WINDOW_MS: i64 = 15 * 60_000;

/// The live ETW process-event source for the record loop, when available.
/// `None` fields mean the watcher is degraded (not elevated / failed to start):
/// the loop then falls back to the plain sleep and snapshot-diff lifecycle
/// exactly as before ETW existed.
#[cfg(windows)]
struct EventSource {
    rx: std::sync::mpsc::Receiver<atlas_collectors::ProcessEvent>,
    watcher: atlas_collectors::ProcessEventWatcher,
}

/// One exact exit from an ETW Stop event: `(pid, exit_ms, exit_status)`. The
/// writer stamps it onto the matching live instance by pid.
type ExitStamp = (u32, i64, Option<i32>);

/// Per-window accumulation of drained ETW events: the raw rows to persist, the
/// exact exits to stamp, and the started/exited counts to feed the cadence
/// controller in place of snapshot diffs.
#[derive(Default)]
struct EventWindow {
    rows: Vec<ProcEventRow>,
    exit_stamps: Vec<ExitStamp>,
    started: u32,
    exited: u32,
}

impl EventWindow {
    fn take(&mut self) -> (Vec<ProcEventRow>, Vec<ExitStamp>) {
        self.started = 0;
        self.exited = 0;
        (
            std::mem::take(&mut self.rows),
            std::mem::take(&mut self.exit_stamps),
        )
    }
}

/// Try to start the live process-event watcher for `record` (start/stop only,
/// no image events). Returns `None` (degraded) on elevation failure or any
/// other error, after logging one clear warning — collection continues either
/// way (the ETW path only sharpens exit timestamps and wake latency).
#[cfg(windows)]
fn try_start_event_source() -> Option<EventSource> {
    use atlas_collectors::{EventError, ProcessEventWatcher};
    match ProcessEventWatcher::start() {
        Ok((watcher, rx)) => {
            tracing::info!(session = watcher.session_name(), "process events: live");
            Some(EventSource { rx, watcher })
        }
        Err(EventError::ElevationRequired) => {
            tracing::warn!(
                "process events degraded: not elevated — exact create/exit timestamps unavailable"
            );
            None
        }
        Err(e) => {
            tracing::warn!("process events degraded: {e}");
            None
        }
    }
}

/// Fold one drained ETW event into the window accumulator: buffer the row for
/// `proc_event`, count it for the cadence controller, and (for a Stop) record
/// an exact exit stamp for the writer to apply by pid.
#[cfg(windows)]
fn fold_event(win: &mut EventWindow, ev: atlas_collectors::ProcessEvent) {
    use atlas_collectors::ProcessEventKind;
    match ev.kind {
        ProcessEventKind::Started {
            parent_pid,
            session_id,
            image_name,
        } => {
            win.started += 1;
            win.rows.push(ProcEventRow {
                ts_ms: ev.ts_ms,
                pid: ev.pid,
                kind: PROC_EVENT_START,
                parent_pid: Some(parent_pid),
                session_id: Some(session_id),
                image_name: Some(image_name),
                exit_status: None,
            });
        }
        ProcessEventKind::Stopped { exit_status } => {
            win.exited += 1;
            win.exit_stamps.push((ev.pid, ev.ts_ms, Some(exit_status)));
            win.rows.push(ProcEventRow {
                ts_ms: ev.ts_ms,
                pid: ev.pid,
                kind: PROC_EVENT_STOP,
                parent_pid: None,
                session_id: None,
                image_name: None,
                exit_status: Some(exit_status),
            });
        }
        // record never enables image events; ignore any that slip through.
        ProcessEventKind::ImageLoaded { .. } => {}
    }
}

/// Wait up to `timeout` for the next ETW event, then drain all currently
/// pending events into `win`. Returning on the first event (rather than always
/// sleeping the full interval) is the event-driven wake: a process start/stop
/// pulls the loop out of even a 15 s idle sleep so churn is sampled at active
/// resolution. A quiet interval simply times out.
///
/// Returns `false` once the channel has disconnected (watcher thread gone).
/// That arm must sleep out the timeout itself: `recv_timeout` on a dead
/// channel returns instantly, and without the sleep the record loop would
/// busy-spin for the rest of the session.
#[cfg(windows)]
fn wait_and_drain_events(
    rx: &std::sync::mpsc::Receiver<atlas_collectors::ProcessEvent>,
    timeout: Duration,
    win: &mut EventWindow,
) -> bool {
    use std::sync::mpsc::RecvTimeoutError;
    match rx.recv_timeout(timeout) {
        Ok(ev) => fold_event(win, ev),
        Err(RecvTimeoutError::Timeout) => return true,
        Err(RecvTimeoutError::Disconnected) => {
            std::thread::sleep(timeout);
            return false;
        }
    }
    // Drain the rest of the burst without blocking.
    while let Ok(ev) = rx.try_recv() {
        fold_event(win, ev);
    }
    true
}

fn cmd_record(
    db_path: PathBuf,
    interval: f64,
    flush_secs: u64,
    duration: Option<u64>,
) -> Result<()> {
    let stop = install_ctrlc();
    record_loop(db_path, interval, flush_secs, duration, stop)
}

/// The record pipeline core, driven by an externally owned `stop` flag so it can
/// be hosted both by the `record` CLI command (Ctrl+C flag) and by the Windows
/// service body (SCM STOP/SHUTDOWN flag). Runs until `stop` flips or `duration`
/// elapses, then drains the writer cleanly.
fn record_loop(
    db_path: PathBuf,
    interval: f64,
    flush_secs: u64,
    duration: Option<u64>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    // The store lives entirely on the writer thread; the sampling loop never
    // touches SQLite (M2). A small bound (4) gives the writer slack without
    // letting a stall balloon memory: past that we drop batches and record a
    // gap rather than block collection.
    let (tx, rx) = sync_channel::<FlushBatch>(4);
    let writer_db = db_path.clone();
    let writer = std::thread::Builder::new()
        .name("atlas-writer".into())
        .spawn(move || writer_thread(writer_db, rx))?;

    // Live process events sharpen exit timestamps and wake sampling instantly
    // on process churn. When degraded, everything below falls back to snapshot
    // diffs + a plain sleep, exactly as before.
    #[cfg(windows)]
    let event_source = try_start_event_source();
    #[cfg(windows)]
    let mut events_live = event_source.is_some();
    #[cfg(not(windows))]
    tracing::warn!("process events degraded: ETW is Windows-only");

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

    let mut tick_buf: Vec<TickSamples> = Vec::new();
    let mut self_acc = SelfAcc::new();
    let mut event_win = EventWindow::default();
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

        // Event-driven wake: when the watcher is live, block on the event
        // channel for at most `next_sleep`; an ETW start/stop returns
        // immediately (so we sample the churn at 1 s resolution even from the
        // 15 s idle tier), then we drain every pending event before sampling.
        // When degraded, this is a plain sleep.
        #[cfg(windows)]
        match event_source.as_ref() {
            Some(src) if events_live => {
                events_live = wait_and_drain_events(&src.rx, next_sleep, &mut event_win);
                if !events_live {
                    tracing::warn!("process event channel closed; falling back to snapshot diffs");
                }
            }
            _ => std::thread::sleep(next_sleep),
        }
        #[cfg(not(windows))]
        std::thread::sleep(next_sleep);

        // Time the sample() call itself — this is the dominant cost of a tick
        // and what the self-metrics report as tick duration.
        let t0 = Instant::now();
        let set = sampler.sample()?;
        let tick_us = t0.elapsed().as_micros() as u64;
        let dt_s = prev_tick.elapsed().as_secs_f64().max(1e-3);
        prev_tick = Instant::now();

        // Prefer real ETW churn counts for the cadence decision when live; fall
        // back to snapshot diffs when degraded. Only true on Windows with an
        // active watcher.
        #[cfg(windows)]
        let live_events = events_live;
        #[cfg(not(windows))]
        let live_events = false;
        let (started_n, exited_n) = if live_events {
            (event_win.started, event_win.exited)
        } else {
            (set.started.len() as u32, set.exited.len() as u32)
        };

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
            started: started_n,
            exited: exited_n,
            max_proc_cpu_permille: max_proc_cpu,
            elapsed: Duration::from_secs_f64(dt_s),
        });
        next_sleep = chosen.max(Duration::from_secs_f64(interval.max(0.25)));

        let own = set.processes.iter().find(|p| p.key.pid == own_pid);
        self_acc.update(own, dt_s, tick_us);
        tick_buf.push(capture_tick(&set));

        if last_flush.elapsed() >= flush_every {
            // When live, exit marking comes from exact ETW Stop events, so the
            // snapshot-diff `exited` set is suppressed to avoid double-marking.
            let snapshot_exited: &[ProcKey] = if live_events { &[] } else { &set.exited };
            let (event_rows, exit_stamps) = event_win.take();
            if let Some(batch) = build_batch(
                &mut tick_buf,
                snapshot_exited,
                &self_acc,
                dropped_pending,
                event_rows,
                exit_stamps,
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

    // Final partial window before shutdown: include any events drained since
    // the last flush so they are not lost on a clean stop.
    let (event_rows, exit_stamps) = event_win.take();
    if let Some(batch) = build_batch(
        &mut tick_buf,
        &[],
        &self_acc,
        dropped_pending,
        event_rows,
        exit_stamps,
    ) {
        if tx.try_send(batch).is_ok() {
            sent_batches += 1;
        } else {
            tracing::warn!("final flush window dropped (writer stalled)");
        }
    }

    // Stop the ETW session cleanly before we tear down the writer.
    #[cfg(windows)]
    if let Some(src) = event_source {
        let dropped = src.watcher.dropped_count();
        if dropped > 0 {
            tracing::warn!(dropped, "some process events were dropped (channel full)");
        }
        if let Err(e) = src.watcher.stop() {
            tracing::warn!("stopping ETW session: {e}");
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

/// Captures one sampled tick into raw per-series values for the writer to
/// append into head blocks. Every process seen this tick contributes its
/// identity + five metric values; system gauges ride alongside.
fn capture_tick(set: &SampleSet) -> TickSamples {
    let sys = SysMetrics {
        cpu_permille: set.system.cpu_permille,
        mem_used: set.system.mem_used,
        mem_total: set.system.mem_total,
        commit_used: set.system.commit_used,
        process_count: set.system.process_count,
        thread_count: set.system.thread_count,
        handle_count: set.system.handle_count,
    };
    let procs = set
        .processes
        .iter()
        .map(|p| {
            let identity = ProcIdentity {
                pid: p.key.pid,
                create_time_100ns: p.key.create_time_100ns,
                parent_pid: p.parent_pid,
                session_id: p.session_id,
                image_name: p.image_name.clone(),
            };
            (identity, ProcMetrics::from_sample(p))
        })
        .collect();
    TickSamples {
        ts_ms: set.ts_ms,
        sys,
        procs,
    }
}

/// Drains the buffered ticks into a self-contained [`FlushBatch`]. Returns
/// `None` when there is nothing to write (no ticks and no events).
fn build_batch(
    tick_buf: &mut Vec<TickSamples>,
    exited: &[ProcKey],
    self_acc: &SelfAcc,
    dropped_before: u64,
    proc_events: Vec<ProcEventRow>,
    exit_stamps: Vec<ExitStamp>,
) -> Option<FlushBatch> {
    // Events alone are enough to warrant a batch even with no samples buffered,
    // so a burst of process churn on an idle machine still lands promptly.
    if tick_buf.is_empty() && proc_events.is_empty() && exit_stamps.is_empty() {
        return None;
    }
    let ts = tick_buf.last().map(|t| t.ts_ms).unwrap_or_else(now_ms);
    Some(FlushBatch {
        agg_ts_ms: ts,
        ticks: std::mem::take(tick_buf),
        exited: exited.to_vec(),
        self_row: self_acc.finish(ts),
        dropped_before,
        proc_events,
        exit_stamps,
    })
}

/// Seal a head block once it reaches ~120 points or ~2 min of span, whichever
/// comes first (tech-stack §4.2: bounded in-memory heads).
const SEAL_MAX_POINTS: u32 = 120;
const SEAL_MAX_AGE_MS: i64 = 120_000;

/// Cardinality guard (tech-stack §4.2): a per-process scope whose last sample is
/// older than this is sealed+drained and forgotten, so a machine that churns
/// through thousands of short-lived processes cannot grow unbounded open heads.
const SCOPE_IDLE_EVICT_MS: i64 = 5 * 60_000;

/// Owns the Gorilla head blocks and per-series bookkeeping for the writer.
/// Separated from the raw store so the append/seal logic is unit-testable.
struct BlockWriter {
    heads: HeadBlocks,
    /// Last-seen wall-clock ms per process scope (row id), for the cardinality
    /// guard. System scope is never evicted.
    scope_last_seen: HashMap<i64, i64>,
}

impl BlockWriter {
    fn new() -> Self {
        Self {
            heads: HeadBlocks::new(),
            scope_last_seen: HashMap::new(),
        }
    }

    /// Appends one tick's system gauges into the six `Sys*` series.
    fn append_sys(&mut self, ts_ms: i64, sys: &SysMetrics) {
        let _ = self.heads.append(
            SeriesKey::system(Metric::SysCpuPermille),
            ts_ms,
            sys.cpu_permille as f64,
        );
        let _ = self.heads.append(
            SeriesKey::system(Metric::SysMemUsed),
            ts_ms,
            sys.mem_used as f64,
        );
        let _ = self.heads.append(
            SeriesKey::system(Metric::SysCommitUsed),
            ts_ms,
            sys.commit_used as f64,
        );
        let _ = self.heads.append(
            SeriesKey::system(Metric::SysProcessCount),
            ts_ms,
            sys.process_count as f64,
        );
        let _ = self.heads.append(
            SeriesKey::system(Metric::SysThreadCount),
            ts_ms,
            sys.thread_count as f64,
        );
        let _ = self.heads.append(
            SeriesKey::system(Metric::SysHandleCount),
            ts_ms,
            sys.handle_count as f64,
        );
    }

    /// Appends one process's five metric values under its resolved `scope`.
    fn append_proc(&mut self, ts_ms: i64, scope: i64, m: &ProcMetrics) {
        let _ = self.heads.append(
            SeriesKey::new(Metric::CpuPermille, scope),
            ts_ms,
            m.cpu_permille as f64,
        );
        let _ = self.heads.append(
            SeriesKey::new(Metric::WorkingSet, scope),
            ts_ms,
            m.working_set as f64,
        );
        let _ = self.heads.append(
            SeriesKey::new(Metric::PrivateBytes, scope),
            ts_ms,
            m.private_bytes as f64,
        );
        let _ = self.heads.append(
            SeriesKey::new(Metric::ReadBps, scope),
            ts_ms,
            m.read_bps as f64,
        );
        let _ = self.heads.append(
            SeriesKey::new(Metric::WriteBps, scope),
            ts_ms,
            m.write_bps as f64,
        );
        self.scope_last_seen.insert(scope, ts_ms);
    }

    /// Seals heads that hit the point/age cap.
    fn drain_sealed(&mut self) -> Vec<atlas_tsdb::EncodedBlock> {
        self.heads.drain_sealed(SEAL_MAX_POINTS, SEAL_MAX_AGE_MS)
    }

    /// Seals+drains a scope's heads (a process exited) and forgets it.
    fn drain_scope(&mut self, scope: i64) -> Vec<atlas_tsdb::EncodedBlock> {
        self.scope_last_seen.remove(&scope);
        self.heads.drain_scope(scope)
    }

    /// Cardinality guard: seal+drain and forget process scopes idle longer than
    /// [`SCOPE_IDLE_EVICT_MS`] relative to `now_ms`.
    fn evict_idle(&mut self, now_ms: i64) -> Vec<atlas_tsdb::EncodedBlock> {
        let stale: Vec<i64> = self
            .scope_last_seen
            .iter()
            .filter(|(scope, last)| {
                **scope != SYSTEM_SCOPE && now_ms - **last >= SCOPE_IDLE_EVICT_MS
            })
            .map(|(scope, _)| *scope)
            .collect();
        let mut out = Vec::new();
        for scope in stale {
            out.extend(self.drain_scope(scope));
        }
        out
    }

    /// Final drain of every open head (shutdown).
    fn drain_all(&mut self) -> Vec<atlas_tsdb::EncodedBlock> {
        self.scope_last_seen.clear();
        self.heads.drain_all()
    }
}

/// Dedicated writer thread: owns the `Store`, the process id cache, and the
/// Gorilla head blocks (M-TSDB). It resolves each tick's process identities to
/// scopes, appends the raw per-tick samples into head blocks, seals and
/// persists blocks, records dropped-window gaps, and sweeps 72 h retention on
/// shutdown. Returns (pruned_proc, pruned_sys) rows from the deprecated tables.
fn writer_thread(
    db_path: PathBuf,
    rx: std::sync::mpsc::Receiver<FlushBatch>,
) -> Result<(usize, usize)> {
    let mut store = Store::open(&db_path)?;
    let mut id_cache: HashMap<ProcKey, i64> = HashMap::new();
    let mut bw = BlockWriter::new();
    // Latest observed total physical memory (bytes), for the memory-pressure
    // detector's percent-of-total threshold. Effectively constant per machine.
    let mut latest_mem_total: u64 = 0;

    for batch in rx {
        // Any windows the sampler dropped since the last landed batch are
        // recorded as a gap so charts can render missing data honestly.
        if batch.dropped_before > 0 {
            store.record_gap(batch.agg_ts_ms, batch.dropped_before, "writer backpressure")?;
        }

        let mut latest_ts = batch.agg_ts_ms;
        // Append every buffered tick into the head blocks, resolving identities
        // → row ids on first sight (the upsert bookkeeping moved off the
        // sampling loop).
        for tick in &batch.ticks {
            latest_ts = latest_ts.max(tick.ts_ms);
            if tick.sys.mem_total > 0 {
                latest_mem_total = tick.sys.mem_total;
            }
            bw.append_sys(tick.ts_ms, &tick.sys);
            for (identity, metrics) in &tick.procs {
                let key = ProcKey {
                    pid: identity.pid,
                    create_time_100ns: identity.create_time_100ns,
                };
                let row_id = match id_cache.get(&key) {
                    Some(id) => *id,
                    None => {
                        let id = store.upsert_process(identity, tick.ts_ms)?;
                        id_cache.insert(key, id);
                        id
                    }
                };
                bw.append_proc(tick.ts_ms, row_id, metrics);
            }
        }

        // Collect blocks to persist: those sealed by point/age, plus any drained
        // by exits below, plus the idle-scope cardinality guard.
        let mut blocks = bw.drain_sealed();

        // Snapshot-diff exits (degraded mode only): mark the instance exited at
        // the flush timestamp and drain its series so nothing is lost.
        for key in &batch.exited {
            if let Some(row_id) = id_cache.remove(key) {
                store.mark_exited(row_id, batch.agg_ts_ms)?;
                blocks.extend(bw.drain_scope(row_id));
            }
        }

        // Exact ETW exits (live mode): stamp the matching live instance by pid
        // with the event's own timestamp and exit status, drain that scope's
        // series, and evict it from the id cache so a later pid reuse gets a
        // fresh row (see stamp_exit_by_pid for why matching is by pid, not the
        // (pid, create_time) key). A stop with no live instance stamps nothing.
        for (pid, exit_ms, exit_status) in &batch.exit_stamps {
            store.stamp_exit_by_pid(*pid, *exit_ms, *exit_status)?;
            // Drain+forget every cached scope for this pid before evicting it.
            let scopes: Vec<i64> = id_cache
                .iter()
                .filter(|(k, _)| k.pid == *pid)
                .map(|(_, id)| *id)
                .collect();
            for scope in scopes {
                blocks.extend(bw.drain_scope(scope));
            }
            id_cache.retain(|k, _| k.pid != *pid);
        }

        // Cardinality guard: shed scopes idle beyond the eviction horizon.
        blocks.extend(bw.evict_idle(latest_ts));

        store.write_batch(&blocks, &batch.proc_events)?;
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
            ticks = batch.ticks.len(),
            blocks = blocks.len(),
            open_series = bw.heads.series_count(),
            event_rows = batch.proc_events.len(),
            exits_stamped = batch.exit_stamps.len(),
            "flushed window"
        );

        // M8: run the detectors over the recent (sealed) window each flush so a
        // long, ongoing incident surfaces during recording. Best-effort: a
        // detection error never disrupts the write path.
        let det_from = latest_ts - DETECT_WINDOW_MS;
        match detectors::run_detection_pass(&store, det_from, latest_ts, latest_mem_total) {
            Ok(n) if n > 0 => tracing::info!(incidents = n, "detection pass upserted incidents"),
            Ok(_) => {}
            Err(e) => tracing::warn!("incident detection pass failed: {e}"),
        }
    }

    // Final drain: seal everything still open so the last samples land.
    let tail = bw.drain_all();
    if !tail.is_empty() {
        store.write_blocks(&tail)?;
    }

    // M8: a final full-span detection pass over everything now persisted. A
    // short recording seals its blocks only here (at drain_all), so this is the
    // pass that catches incidents from brief `record` runs. Idempotent with the
    // per-flush passes.
    let final_to = now_ms();
    let final_from = final_to - RETENTION_HOURS * 3_600_000;
    match detectors::run_detection_pass(&store, final_from, final_to, latest_mem_total) {
        Ok(n) => tracing::info!(incidents = n, "final detection pass complete"),
        Err(e) => tracing::warn!("final incident detection pass failed: {e}"),
    }

    let cutoff = now_ms() - RETENTION_HOURS * 3_600_000;
    let pruned = store.apply_retention(cutoff)?;
    let blocks_pruned = store.apply_block_retention(cutoff)?;
    tracing::info!(blocks_pruned, "block retention swept");
    Ok(pruned)
}

/// Exit code returned when the ETW session cannot start because the process is
/// not elevated — lets callers/scripts distinguish this from other failures.
const EXIT_ELEVATION_REQUIRED: i32 = 2;

#[cfg(windows)]
fn cmd_events(images: bool) -> Result<()> {
    use atlas_collectors::{EventError, ProcessEventWatcher, WatcherOptions};

    let stop = install_ctrlc();

    let (watcher, rx) = match ProcessEventWatcher::start_with_options(WatcherOptions { images }) {
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
fn cmd_events(_images: bool) -> Result<()> {
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
        ProcessEventKind::ImageLoaded {
            image_base,
            image_size,
            image_name,
        } => format!(
            "[{ts}] IMAGE pid={} base={image_base:#x} size={image_size} {image_name}",
            ev.pid
        ),
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

/// The shared-memory ring discriminator for a given pipe discriminator. Uses
/// the same token as the pipe (or the current username when unset) so `serve`
/// and `ring-read` rendezvous on one flag.
#[cfg(windows)]
fn ring_discriminator(pipe: Option<String>) -> String {
    pipe.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        std::env::var("USERNAME")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "session".to_string())
    })
}

/// `serve`: host AtlasQuery + AtlasControl over the named pipe until Ctrl+C.
#[cfg(windows)]
fn cmd_serve(pipe: Option<String>, db: PathBuf) -> Result<()> {
    let stop = install_ctrlc();
    serve_loop(pipe, db, stop)
}

/// The serve core, driven by an externally owned `stop` flag so it can be hosted
/// both by the `serve` CLI command (Ctrl+C flag) and by the Windows service body
/// (SCM STOP/SHUTDOWN flag). Blocks until `stop` flips, then drains cleanly.
#[cfg(windows)]
fn serve_loop(pipe: Option<String>, db: PathBuf, stop: Arc<AtomicBool>) -> Result<()> {
    use atlas_ipc::{AtlasControlServer, AtlasQueryServer};

    let pipe_disc = pipe.clone();
    let name = resolve_pipe_name(pipe);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // The ring discriminator mirrors the pipe discriminator so a `ring-read`
    // client with the same `--pipe` flag rendezvous with this server.
    let ring_disc = ring_discriminator(pipe_disc);
    rt.block_on(async move {
        let service = ipc::QueryService::start(&ring_disc, db)?;
        let handle = std::sync::Arc::new(service);
        // The broker shares the query service's store handle so both the audit
        // log and the history queries use the same connection.
        let broker = std::sync::Arc::new(broker::BrokerService::new(handle.store()));
        let router = tonic::transport::Server::builder()
            .add_service(AtlasQueryServer::from_arc(handle.clone()))
            .add_service(AtlasControlServer::from_arc(broker));

        tracing::info!(pipe = %name, "AtlasQuery + AtlasControl serving");
        println!("Serving AtlasQuery + AtlasControl on {name}");

        // Shut down when the shared stop flag flips (Ctrl+C in the CLI path, or
        // the SCM STOP/SHUTDOWN control in the service path). Poll at ~10 Hz.
        let shutdown = async move {
            while !stop.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        let result = atlas_ipc::serve(&name, router, shutdown).await;
        handle.shutdown();
        result
    })?;

    tracing::info!("Atlas server stopped");
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

/// `ring-read`: attach to the shared-memory live ring published by a running
/// `serve` and print the header + top rows. Lock-free seqlock read path.
#[cfg(windows)]
fn cmd_ring_read(pipe: Option<String>, limit: usize, watch: bool) -> Result<()> {
    use atlas_ipc::RingReader;

    let disc = ring_discriminator(pipe);
    let reader = RingReader::open(&disc).map_err(|e| {
        anyhow::anyhow!(
            "attach to live ring '{}': {e}\nIs `serve` running with a matching --pipe?",
            atlas_ipc::section_name(&disc)
        )
    })?;

    if !watch {
        match reader.snapshot() {
            Some(snap) => render_ring(&snap, limit),
            None => println!("Ring writer busy (seqlock retries exhausted); try again."),
        }
        return Ok(());
    }

    let stop = install_ctrlc();
    println!("Reading live ring '{}' (Ctrl+C to stop)", section(&disc));
    while !stop.load(Ordering::SeqCst) {
        if let Some(snap) = reader.snapshot() {
            // Clear-ish repaint: a couple of blank lines keep the block readable
            // in a plain console without pulling in a TUI dependency.
            println!("\n");
            render_ring(&snap, limit);
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

/// Section name helper for display.
#[cfg(windows)]
fn section(disc: &str) -> String {
    atlas_ipc::section_name(disc)
}

/// Renders a ring snapshot as a header line + top rows, mirroring `print_snapshot`.
#[cfg(windows)]
fn render_ring(snap: &atlas_ipc::RingSnapshot, limit: usize) {
    println!(
        "Ring @ {} | CPU {:.1}%  Memory {:.1}/{:.1} GB  Commit {:.1}/{:.1} GB  {} processes, {} threads, {} handles",
        format_ts(snap.ts_ms),
        snap.cpu_permille as f64 / 10.0,
        gb(snap.mem_used),
        gb(snap.mem_total),
        gb(snap.commit_used),
        gb(snap.commit_limit),
        snap.process_count,
        snap.thread_count,
        snap.handle_count
    );
    println!(
        "{:>7} {:<30} {:>6} {:>9} {:>9} {:>11} {:>11}",
        "PID", "NAME", "CPU%", "WS MB", "PRIV MB", "READ/s", "WRITE/s"
    );
    for r in snap.rows.iter().take(limit) {
        println!(
            "{:>7} {:<30} {:>6.1} {:>9.1} {:>9.1} {:>11} {:>11}",
            r.pid,
            truncate(&r.name, 30),
            r.cpu_permille as f64 / 10.0,
            mb(r.working_set),
            mb(r.private_bytes),
            rate(r.read_bps),
            rate(r.write_bps),
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

#[cfg(not(windows))]
fn cmd_ring_read(_pipe: Option<String>, _limit: usize, _watch: bool) -> Result<()> {
    anyhow::bail!("the `ring-read` command requires Windows shared memory");
}

/// Metrics accumulated across an `overhead` run, independent of what lands in
/// the store. CPU/working-set come from the own-process sample each tick; tick
/// timing measures the `sample()` call itself.
struct OverheadMetrics {
    ticks: u64,
    cpu_permille_sum: f64,
    cpu_weight_s: f64,
    cpu_permille_max: u32,
    working_set_max: u64,
    working_set_last: u64,
    tick_us_sum: u64,
    tick_us_max: u64,
}

impl OverheadMetrics {
    fn new() -> Self {
        Self {
            ticks: 0,
            cpu_permille_sum: 0.0,
            cpu_weight_s: 0.0,
            cpu_permille_max: 0,
            working_set_max: 0,
            working_set_last: 0,
            tick_us_sum: 0,
            tick_us_max: 0,
        }
    }

    fn record(&mut self, own: Option<&ProcSample>, dt_s: f64, tick_us: u64) {
        self.ticks += 1;
        self.tick_us_sum += tick_us;
        self.tick_us_max = self.tick_us_max.max(tick_us);
        if let Some(p) = own {
            self.cpu_permille_sum += p.cpu_permille as f64 * dt_s;
            self.cpu_weight_s += dt_s;
            self.cpu_permille_max = self.cpu_permille_max.max(p.cpu_permille);
            self.working_set_max = self.working_set_max.max(p.working_set);
            self.working_set_last = p.working_set;
        }
    }

    fn cpu_avg_permille(&self) -> f64 {
        if self.cpu_weight_s > 0.0 {
            self.cpu_permille_sum / self.cpu_weight_s
        } else {
            0.0
        }
    }

    fn tick_us_avg(&self) -> u64 {
        self.tick_us_sum.checked_div(self.ticks).unwrap_or(0)
    }
}

/// PRD §12 budgets the harness evaluates against (tech-stack §10).
const BUDGET_CPU_PERMILLE: f64 = 2.0; // < 0.2% idle average.
const BUDGET_WS_BYTES: u64 = 100 * 1024 * 1024; // < 100 MB service standard mode.

/// `overhead`: run the real record pipeline against a TEMP database for
/// `duration` seconds and report own cost against the PRD budgets. Always
/// returns Ok(()) so the process exits 0 — informational until M9 makes it a
/// gate. The temp database is deleted on the way out.
fn cmd_overhead(duration: u64, interval: f64, flush_secs: u64, json: bool) -> Result<()> {
    let stop = install_ctrlc();

    // A unique temp DB so parallel runs never collide; deleted in all exit
    // paths below (including the `?` early returns, via the guard).
    let db_path = std::env::temp_dir().join(format!(
        "atlas-overhead-{}-{}.db",
        std::process::id(),
        now_ms()
    ));
    let _guard = TempDbGuard(db_path.clone());

    // Writer thread + channel, exactly as `record`.
    let (tx, rx) = sync_channel::<FlushBatch>(4);
    let writer_db = db_path.clone();
    let writer = std::thread::Builder::new()
        .name("atlas-overhead-writer".into())
        .spawn(move || writer_thread(writer_db, rx))?;

    #[cfg(windows)]
    let event_source = try_start_event_source();
    #[cfg(windows)]
    let etw_live = event_source.is_some();
    #[cfg(windows)]
    let mut events_live = event_source.is_some();
    #[cfg(not(windows))]
    let etw_live = false;

    let mut sampler = Sampler::new()?;
    let own_pid = std::process::id();
    let mut cadence = CadenceController::new();

    if !json {
        println!(
            "Running overhead harness for {duration}s (temp db: {}) ...",
            db_path.display()
        );
    }

    let started = Instant::now();
    let flush_every = Duration::from_secs(flush_secs.max(2));
    let mut last_flush = Instant::now();
    let mut next_sleep = Duration::from_secs_f64(interval.max(0.25));

    let mut tick_buf: Vec<TickSamples> = Vec::new();
    let mut self_acc = SelfAcc::new();
    let mut event_win = EventWindow::default();
    let mut prev_tick = Instant::now();
    let mut dropped_pending = 0u64;

    let mut metrics = OverheadMetrics::new();
    let mut flush_windows = 0u64;

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if started.elapsed() >= Duration::from_secs(duration) {
            break;
        }

        #[cfg(windows)]
        match event_source.as_ref() {
            Some(src) if events_live => {
                events_live = wait_and_drain_events(&src.rx, next_sleep, &mut event_win);
            }
            _ => std::thread::sleep(next_sleep),
        }
        #[cfg(not(windows))]
        std::thread::sleep(next_sleep);

        let t0 = Instant::now();
        let set = sampler.sample()?;
        let tick_us = t0.elapsed().as_micros() as u64;
        let dt_s = prev_tick.elapsed().as_secs_f64().max(1e-3);
        prev_tick = Instant::now();

        let own = set.processes.iter().find(|p| p.key.pid == own_pid);
        metrics.record(own, dt_s, tick_us);

        #[cfg(windows)]
        let live_events = events_live;
        #[cfg(not(windows))]
        let live_events = false;
        let (started_n, exited_n) = if live_events {
            (event_win.started, event_win.exited)
        } else {
            (set.started.len() as u32, set.exited.len() as u32)
        };
        let max_proc_cpu = set
            .processes
            .iter()
            .map(|p| p.cpu_permille)
            .max()
            .unwrap_or(0);
        let chosen = cadence.next_interval(Tick {
            sys_cpu_permille: set.system.cpu_permille,
            started: started_n,
            exited: exited_n,
            max_proc_cpu_permille: max_proc_cpu,
            elapsed: Duration::from_secs_f64(dt_s),
        });
        next_sleep = chosen.max(Duration::from_secs_f64(interval.max(0.25)));

        self_acc.update(own, dt_s, tick_us);
        tick_buf.push(capture_tick(&set));

        if last_flush.elapsed() >= flush_every {
            let snapshot_exited: &[ProcKey] = if live_events { &[] } else { &set.exited };
            let (event_rows, exit_stamps) = event_win.take();
            if let Some(batch) = build_batch(
                &mut tick_buf,
                snapshot_exited,
                &self_acc,
                dropped_pending,
                event_rows,
                exit_stamps,
            ) {
                match tx.try_send(batch) {
                    Ok(()) => {
                        flush_windows += 1;
                        dropped_pending = 0;
                    }
                    Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                        dropped_pending += 1;
                    }
                }
            }
            self_acc = SelfAcc::new();
            last_flush = Instant::now();
        }
    }

    let elapsed = started.elapsed();

    // Final partial window.
    let (event_rows, exit_stamps) = event_win.take();
    if let Some(batch) = build_batch(
        &mut tick_buf,
        &[],
        &self_acc,
        dropped_pending,
        event_rows,
        exit_stamps,
    ) {
        if tx.try_send(batch).is_ok() {
            flush_windows += 1;
        }
    }

    #[cfg(windows)]
    if let Some(src) = event_source {
        if let Err(e) = src.watcher.stop() {
            tracing::warn!("stopping ETW session: {e}");
        }
    }

    // Drain and join the writer, then size the database on disk.
    drop(tx);
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("writer thread panicked"))??;

    let db_bytes = db_on_disk_bytes(&db_path);

    // Steady-state projection: the encoded sample-block payload is the honest
    // driver of disk growth. A short harness run's *file* size is dominated by
    // fixed SQLite page/WAL overhead plus the one-time final drain of every open
    // head into tiny (sub-120-point) blocks, so extrapolating it overstates the
    // rate. Projecting from bytes/sample × sample rate reflects the sealed-block
    // steady state the store actually settles into. Read before the guard runs.
    let block_stats = Store::open(&db_path).ok().and_then(|s| {
        let bytes = s.sample_storage_bytes().ok()?;
        let samples = s.sample_count().ok()?;
        Some((bytes, samples))
    });

    if json {
        print_overhead_json(
            &metrics,
            elapsed,
            flush_windows,
            db_bytes,
            block_stats,
            etw_live,
        );
    } else {
        print_overhead_report(
            &metrics,
            elapsed,
            flush_windows,
            db_bytes,
            block_stats,
            etw_live,
            interval,
            flush_secs,
        );
    }

    // `_guard` deletes the temp db here on drop.
    Ok(())
}

/// Emits the single machine-readable overhead line the CI perf gate parses
/// (M9). Field names are STABLE — the gate keys off them; do not rename without
/// updating `.github/workflows/perf.yml`. Percentages are derived from permille
/// (÷10); working set + steady-state disk come from the same figures the human
/// report prints.
fn print_overhead_json(
    m: &OverheadMetrics,
    elapsed: Duration,
    flush_windows: u64,
    db_bytes: u64,
    block_stats: Option<(u64, u64)>,
    etw_live: bool,
) {
    let secs = elapsed.as_secs_f64().max(1e-3);
    let cpu_avg_pct = m.cpu_avg_permille() / 10.0;
    let cpu_max_pct = m.cpu_permille_max as f64 / 10.0;
    let ws = m.working_set_max.max(m.working_set_last);
    let ws_mb = mb(ws);

    let (bytes_per_sample, mb_per_day_steadystate) = match block_stats {
        Some((payload_bytes, samples)) if samples > 0 => {
            let bps = payload_bytes as f64 / samples as f64;
            let samples_per_s = samples as f64 / secs;
            let mb_per_day = bps * samples_per_s * 86_400.0 / (1024.0 * 1024.0);
            (bps, mb_per_day)
        }
        _ => (0.0, 0.0),
    };

    let cpu_budget_pct = BUDGET_CPU_PERMILLE / 10.0;
    let ws_budget_mb = (BUDGET_WS_BYTES / (1024 * 1024)) as f64;
    // The working-set gate is authoritative; the CPU pass is advisory on shared
    // CI (documented in perf.yml). Report both so the gate can choose.
    let pass_cpu = m.cpu_weight_s > 0.0 && m.cpu_avg_permille() < BUDGET_CPU_PERMILLE;
    let pass_ws = ws > 0 && ws < BUDGET_WS_BYTES;

    let line = serde_json::json!({
        "duration_s": (secs * 10.0).round() / 10.0,
        "own_cpu_avg_pct": round3(cpu_avg_pct),
        "own_cpu_max_pct": round3(cpu_max_pct),
        "own_working_set_mb": round3(ws_mb),
        "tick_avg_ms": round3(m.tick_us_avg() as f64 / 1000.0),
        "tick_max_ms": round3(m.tick_us_max as f64 / 1000.0),
        "flush_windows": flush_windows,
        "db_bytes": db_bytes,
        "mb_per_day_steadystate": round3(mb_per_day_steadystate),
        "bytes_per_sample": round3(bytes_per_sample),
        "etw": if etw_live { "live" } else { "degraded" },
        "budgets": {
            "cpu_avg_pct": cpu_budget_pct,
            "working_set_mb": ws_budget_mb,
        },
        "pass": {
            "cpu": pass_cpu,
            "working_set": pass_ws,
        },
    });
    println!("{line}");
}

/// Rounds to 3 decimals for stable, compact JSON output.
fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The Windows service name (SCM key) and display name.
const SERVICE_NAME: &str = "AtlasService";
const SERVICE_DISPLAY_NAME: &str = "System Atlas Collection Service";

/// Production store path for the service body: `%ProgramData%\SystemAtlas\atlas.db`
/// (tech-stack §7 — the service runs as LocalSystem, so per-user LOCALAPPDATA is
/// wrong). Falls back to the dev path if PROGRAMDATA is unset.
fn default_service_db_path() -> PathBuf {
    match std::env::var_os("PROGRAMDATA") {
        Some(pd) => PathBuf::from(pd).join("SystemAtlas").join("atlas.db"),
        None => default_db_path(),
    }
}

/// `service`: install / uninstall / run / status the Windows service host (M9).
#[cfg(windows)]
fn cmd_service(cmd: ServiceCmd) -> Result<()> {
    use service_ctl::{InstallOutcome, QueryOutcome, RunOutcome, UninstallOutcome};

    match cmd {
        ServiceCmd::Install => match service_ctl::install(SERVICE_NAME, SERVICE_DISPLAY_NAME)? {
            InstallOutcome::Created => {
                println!(
                    "Installed service '{SERVICE_NAME}' (auto-start, crash-restart: restart after 5 s, 3 attempts, reset window 1 day)."
                );
                Ok(())
            }
            InstallOutcome::AlreadyExists => {
                println!("Service '{SERVICE_NAME}' is already installed.");
                Ok(())
            }
            InstallOutcome::AccessDenied => {
                eprintln!(
                    "Installing a service requires administrator rights. \
                     Rerun `service install` from an elevated (Run as administrator) terminal."
                );
                std::process::exit(EXIT_ELEVATION_REQUIRED);
            }
        },
        ServiceCmd::Uninstall => match service_ctl::uninstall(SERVICE_NAME)? {
            UninstallOutcome::Deleted => {
                println!("Uninstalled service '{SERVICE_NAME}'.");
                Ok(())
            }
            UninstallOutcome::NotInstalled => {
                println!("Service '{SERVICE_NAME}' is not installed.");
                Ok(())
            }
            UninstallOutcome::AccessDenied => {
                eprintln!(
                    "Uninstalling a service requires administrator rights. \
                     Rerun `service uninstall` from an elevated terminal."
                );
                std::process::exit(EXIT_ELEVATION_REQUIRED);
            }
        },
        ServiceCmd::Status => {
            match service_ctl::query_status(SERVICE_NAME)? {
                QueryOutcome::Status(s) => {
                    println!(
                        "Service '{SERVICE_NAME}': {} (pid {}, exit code {})",
                        service_ctl::state_label(s.current_state),
                        s.pid,
                        s.win32_exit_code
                    );
                    Ok(())
                }
                QueryOutcome::NotInstalled => {
                    println!("Service '{SERVICE_NAME}' is not installed. Run `service install` (elevated).");
                    Ok(())
                }
                QueryOutcome::AccessDenied => {
                    eprintln!("Querying the service requires more access than this token has.");
                    std::process::exit(EXIT_ELEVATION_REQUIRED);
                }
            }
        }
        ServiceCmd::Run => match service_ctl::run_service(SERVICE_NAME, hosted_service_workload)? {
            RunOutcome::Completed => {
                tracing::info!("service dispatcher returned; process exiting");
                Ok(())
            }
            RunOutcome::NotUnderScm => {
                eprintln!(
                    "`service run` must be launched by the Service Control Manager, not from a \
                     console. Use `service install` (elevated) then start it via services.msc / \
                     `sc start {SERVICE_NAME}`. For a foreground collection run, use `record` or `serve`."
                );
                std::process::exit(EXIT_SERVICE_NOT_UNDER_SCM);
            }
        },
    }
}

#[cfg(not(windows))]
fn cmd_service(_cmd: ServiceCmd) -> Result<()> {
    anyhow::bail!("the Windows service host is only available on Windows");
}

/// Exit code when `service run` is launched outside the SCM (console run).
const EXIT_SERVICE_NOT_UNDER_SCM: i32 = 3;

/// The service body: run the collection pipeline (`record`) on a background
/// thread and host the gRPC/ring `serve` on this thread, both keyed to the SCM
/// stop flag. When the SCM signals STOP/SHUTDOWN the flag flips, `serve` drains,
/// and the record writer is joined so the last window lands (tech-stack §4.1).
#[cfg(windows)]
fn hosted_service_workload(stop: Arc<AtomicBool>) -> Result<()> {
    let db = default_service_db_path();
    if let Some(parent) = db.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    tracing::info!(db = %db.display(), "service workload starting (collection + serve)");

    // Collection on a background thread (runs until `stop` flips).
    let rec_stop = stop.clone();
    let rec_db = db.clone();
    let rec = std::thread::Builder::new()
        .name("atlas-svc-record".into())
        .spawn(move || record_loop(rec_db, 1.0, 15, None, rec_stop))?;

    // Serve on this thread until `stop` flips.
    let serve_res = serve_loop(None, db, stop.clone());

    // Make sure the collection thread is told to stop, then join it.
    stop.store(true, Ordering::SeqCst);
    let rec_res = rec
        .join()
        .map_err(|_| anyhow::anyhow!("record thread panicked"))?;

    serve_res.and(rec_res)
}

/// `soak`: run the real record pipeline for N minutes while periodically
/// observing this process's OWN working set + handle count, then fit an RSS
/// slope and peak handle growth and print a PASS/FAIL verdict (M9, PRD §12.2).
///
/// The record pipeline runs in-process on a background thread, so this process's
/// footprint *is* the collection footprint being watched. A short run (a few
/// minutes) suits CI; a long run (e.g. `--minutes 4320` for 72 h) is the manual
/// leak soak. Returns a non-zero exit (via `Err`) on a FAIL verdict so CI gates.
fn cmd_soak(
    minutes: u64,
    sample_secs: u64,
    interval: f64,
    flush_secs: u64,
    slope_threshold: f64,
    handle_threshold: i64,
    warmup_secs: f64,
) -> Result<()> {
    let stop = install_ctrlc();
    let duration_s = (minutes.max(1)) * 60;
    let period = Duration::from_secs(sample_secs.max(1));

    // Real record pipeline against a temp db, deleted on the way out.
    let db_path =
        std::env::temp_dir().join(format!("atlas-soak-{}-{}.db", std::process::id(), now_ms()));
    let _guard = TempDbGuard(db_path.clone());

    let rec_stop = stop.clone();
    let rec_db = db_path.clone();
    let rec = std::thread::Builder::new()
        .name("atlas-soak-record".into())
        .spawn(move || record_loop(rec_db, interval, flush_secs, Some(duration_s), rec_stop))?;

    println!(
        "Soak: running the record pipeline for {minutes} min, sampling own RSS/handles every {}s ...",
        period.as_secs()
    );

    // Self-observation loop: a lightweight Sampler read every `period`, extracting
    // this process's own working set + handle count.
    let own_pid = std::process::id();
    let mut sampler = Sampler::new()?;
    let _ = sampler.sample(); // prime (first read seeds CPU deltas; ws/handles valid)
    let started = Instant::now();
    let mut samples: Vec<soak::SoakSample> = Vec::new();
    let mut next = Instant::now();

    while !stop.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(duration_s) {
        std::thread::sleep(Duration::from_millis(200));
        if Instant::now() < next {
            continue;
        }
        next = Instant::now() + period;
        let set = sampler.sample()?;
        if let Some(p) = set.processes.iter().find(|p| p.key.pid == own_pid) {
            samples.push(soak::SoakSample {
                t_s: started.elapsed().as_secs_f64(),
                rss_bytes: p.working_set,
                handles: p.handle_count,
            });
        }
    }

    // Wind down the collection thread and join it.
    stop.store(true, Ordering::SeqCst);
    rec.join()
        .map_err(|_| anyhow::anyhow!("record thread panicked"))??;

    let verdict = soak::analyze(&samples, warmup_secs, slope_threshold, handle_threshold);
    print_soak_verdict(&verdict, minutes);

    if !verdict.pass {
        anyhow::bail!(
            "soak FAILED: RSS slope {:.2} MB/hr (threshold {:.2}), peak handle growth {} (threshold {})",
            verdict.rss_slope_mb_per_hour,
            verdict.slope_threshold_mb_per_hour,
            verdict.peak_handle_growth,
            verdict.handle_growth_threshold
        );
    }
    Ok(())
}

/// Renders the soak verdict block.
fn print_soak_verdict(v: &soak::SoakVerdict, minutes: u64) {
    let verdict = if v.insufficient {
        "INSUFFICIENT"
    } else if v.pass {
        "PASS"
    } else {
        "FAIL"
    };
    println!();
    println!("======== Atlas soak report ========");
    println!(
        "run length      {minutes} min ({} self-samples, {} after {:.0}s warmup)",
        v.samples, v.analyzed_samples, v.warmup_s
    );
    println!(
        "RSS             first {:.1} MB   peak {:.1} MB   (post-warmup window)",
        v.rss_first_mb, v.rss_peak_mb
    );
    println!(
        "RSS slope       {:.3} MB/hour   [threshold {:.2} MB/hour]",
        v.rss_slope_mb_per_hour, v.slope_threshold_mb_per_hour
    );
    println!(
        "fitted rise     {:.2} MB over the window   [materiality floor {:.1} MB]",
        v.fitted_rise_mb,
        soak::DEFAULT_MIN_RSS_RISE_MB
    );
    println!(
        "handles         first {}   peak {}   growth {}   [threshold {}]",
        v.handles_first, v.handles_peak, v.peak_handle_growth, v.handle_growth_threshold
    );
    if v.insufficient {
        println!(
            "verdict         INSUFFICIENT (need >= 2 post-warmup samples; lengthen the run or lower --warmup-secs)"
        );
    } else {
        println!("verdict         {verdict}");
    }
    println!("===================================");
}

/// Deletes the temp database (and its `-wal`/`-shm` sidecars) on drop, so every
/// exit path of `cmd_overhead` cleans up.
struct TempDbGuard(PathBuf);

impl Drop for TempDbGuard {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let p = if suffix.is_empty() {
                self.0.clone()
            } else {
                PathBuf::from(format!("{}{suffix}", self.0.display()))
            };
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Sums the SQLite database file and its WAL/SHM sidecars on disk (bytes).
fn db_on_disk_bytes(db_path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    for suffix in ["", "-wal", "-shm"] {
        let p = if suffix.is_empty() {
            db_path.to_path_buf()
        } else {
            PathBuf::from(format!("{}{suffix}", db_path.display()))
        };
        if let Ok(meta) = std::fs::metadata(&p) {
            total += meta.len();
        }
    }
    total
}

/// Renders the compact overhead report block and PASS/FAIL/N.A. verdicts.
#[allow(clippy::too_many_arguments)]
fn print_overhead_report(
    m: &OverheadMetrics,
    elapsed: Duration,
    flush_windows: u64,
    db_bytes: u64,
    block_stats: Option<(u64, u64)>,
    etw_live: bool,
    interval: f64,
    flush_secs: u64,
) {
    let secs = elapsed.as_secs_f64().max(1e-3);
    let cpu_avg = m.cpu_avg_permille();
    let cpu_pct_avg = cpu_avg / 10.0;
    let cpu_pct_max = m.cpu_permille_max as f64 / 10.0;
    let ws = m.working_set_max.max(m.working_set_last);

    // Extrapolate disk writes/day from bytes actually written during the run.
    let mb_per_day = (db_bytes as f64 / (1024.0 * 1024.0)) * (86_400.0 / secs);

    // Steady-state projection from the encoded sample-block payload: bytes/
    // sample × the sample production rate. This is the honest disk-growth figure
    // the M-TSDB store settles into once blocks seal at the point cap (a short
    // run's raw file size is dominated by fixed SQLite overhead + the one-time
    // final drain of open heads). Samples/s is derived from the ticks actually
    // taken so it tracks the adaptive cadence during the run.
    let steady = block_stats.map(|(payload_bytes, samples)| {
        let bytes_per_sample = if samples > 0 {
            payload_bytes as f64 / samples as f64
        } else {
            0.0
        };
        let samples_per_s = samples as f64 / secs;
        let payload_mb_per_day = bytes_per_sample * samples_per_s * 86_400.0 / (1024.0 * 1024.0);
        (bytes_per_sample, payload_mb_per_day)
    });

    // Verdicts. CPU budget only meaningful once a few ticks landed own-process
    // CPU; otherwise report N.A. rather than a false PASS.
    let cpu_verdict = if m.cpu_weight_s <= 0.0 {
        "N.A."
    } else if cpu_avg < BUDGET_CPU_PERMILLE {
        "PASS"
    } else {
        "FAIL"
    };
    let ws_verdict = if ws == 0 {
        "N.A."
    } else if ws < BUDGET_WS_BYTES {
        "PASS"
    } else {
        "FAIL"
    };

    println!();
    println!("======== Atlas overhead report ========");
    println!(
        "duration        {:.1}s ({} ticks, interval floor {:.2}s, flush {}s)",
        secs, m.ticks, interval, flush_secs
    );
    println!(
        "own CPU avg     {:.3}%   [budget < {:.1}%: {}]",
        cpu_pct_avg,
        BUDGET_CPU_PERMILLE / 10.0,
        cpu_verdict
    );
    println!("own CPU max     {cpu_pct_max:.3}%");
    println!(
        "own working set {:.1} MB   [budget < {} MB: {}]",
        mb(ws),
        BUDGET_WS_BYTES / (1024 * 1024),
        ws_verdict
    );
    println!(
        "sampler tick    avg {:.3} ms   max {:.3} ms",
        m.tick_us_avg() as f64 / 1000.0,
        m.tick_us_max as f64 / 1000.0
    );
    println!(
        "flush windows   {flush_windows} written   db on disk {:.2} MB   ~{:.1} MB/day (cold-file extrapolation)",
        mb(db_bytes),
        mb_per_day
    );
    match steady {
        Some((bytes_per_sample, payload_mb_per_day)) => println!(
            "sample blocks   {:.3} bytes/sample   ~{:.1} MB/day steady-state payload",
            bytes_per_sample, payload_mb_per_day
        ),
        None => println!("sample blocks   (no blocks written)"),
    }
    println!(
        "ETW events      {}",
        if etw_live {
            "LIVE (elevated)"
        } else {
            "DEGRADED (not elevated) — process create/exit timestamps not measured; \
             overhead reflects the snapshot+storage path only"
        }
    );
    println!("=======================================");
}

/// Parses a CLI metric token into an [`atlas_tsdb::Metric`]. Accepts short
/// aliases for both the system gauges and the per-process series.
fn parse_metric(token: &str) -> Option<Metric> {
    Some(match token.to_ascii_lowercase().as_str() {
        "sys-cpu" | "sys_cpu" => Metric::SysCpuPermille,
        "sys-mem" | "sys_mem" => Metric::SysMemUsed,
        "sys-commit" | "sys_commit" => Metric::SysCommitUsed,
        "sys-procs" | "sys-proc" | "sys_procs" => Metric::SysProcessCount,
        "cpu" => Metric::CpuPermille,
        "ws" | "working-set" => Metric::WorkingSet,
        "priv" | "private" => Metric::PrivateBytes,
        "read" | "read-bps" => Metric::ReadBps,
        "write" | "write-bps" => Metric::WriteBps,
        _ => return None,
    })
}

/// `history`: decimate a metric series over a look-back window and print the
/// buckets. Exercises the store's `query_range` (the AtlasQuery RPC's backend).
fn cmd_history(
    db_path: PathBuf,
    metric: &str,
    scope: i64,
    minutes: u64,
    buckets: u32,
) -> Result<()> {
    let m = parse_metric(metric).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown metric '{metric}'. Try: sys-cpu sys-mem sys-commit sys-procs cpu ws priv read write"
        )
    })?;
    let store = Store::open(&db_path)?;
    let now = now_ms();
    let from = now - (minutes as i64) * 60_000;
    let rows = store.query_range(m, scope, from, now, buckets)?;
    println!(
        "History for {metric} (scope {scope}) over the last {minutes} min — {} bucket(s), source {}",
        rows.len(),
        db_path.display()
    );
    if rows.is_empty() {
        println!("(no samples in range — run `record` first, or widen --minutes)");
        return Ok(());
    }
    println!(
        "{:>15} {:>12} {:>12} {:>12} {:>8}",
        "START (t-of-day)", "MIN", "MAX", "AVG", "SAMPLES"
    );
    for b in rows {
        println!(
            "{:>15} {:>12.2} {:>12.2} {:>12.2} {:>8}",
            format_ts(b.start_ms),
            b.min,
            b.max,
            b.avg,
            b.samples
        );
    }
    Ok(())
}

/// `search`: run the store search and print the three hit lists.
fn cmd_search(db_path: PathBuf, query: &str, limit: u32) -> Result<()> {
    let store = Store::open(&db_path)?;
    let hits = store.search(query, limit)?;
    println!(
        "Search '{query}' (FTS5: {}) — {} process, {} event, {} bookmark hit(s), source {}",
        if store.has_fts5() {
            "on"
        } else {
            "LIKE fallback"
        },
        hits.processes.len(),
        hits.events.len(),
        hits.bookmarks.len(),
        db_path.display()
    );
    for p in &hits.processes {
        println!(
            "  proc  pid={:>6} {:<28} {}",
            p.pid,
            truncate(&p.image_name, 28),
            if p.live { "live" } else { "exited" }
        );
    }
    for e in &hits.events {
        let kind = if e.kind == PROC_EVENT_START as u32 {
            "start"
        } else {
            "stop"
        };
        println!(
            "  event {:>5} pid={:>6} {}",
            kind,
            e.pid,
            truncate(&e.image_name, 28)
        );
    }
    for b in &hits.bookmarks {
        println!(
            "  bmark id={:>4} [{}] {}",
            b.id,
            format_ts(b.ts_ms),
            b.label
        );
    }
    Ok(())
}

/// `bookmark add|list`.
fn cmd_bookmark(db_path: PathBuf, cmd: BookmarkCmd) -> Result<()> {
    let store = Store::open(&db_path)?;
    match cmd {
        BookmarkCmd::Add { label, at } => {
            let ts = at.unwrap_or_else(now_ms);
            let id = store.create_bookmark(ts, &label)?;
            println!("Added bookmark #{id} at {} — \"{label}\"", format_ts(ts));
        }
        BookmarkCmd::List { from, to } => {
            let from = from.unwrap_or(i64::MIN);
            let to = to.unwrap_or(i64::MAX);
            let rows = store.list_bookmarks(from, to)?;
            if rows.is_empty() {
                println!("No bookmarks in range ({}).", db_path.display());
                return Ok(());
            }
            println!("{} bookmark(s):", rows.len());
            for b in rows {
                println!("  #{:<4} [{}] {}", b.id, format_ts(b.ts_ms), b.label);
            }
        }
    }
    Ok(())
}

/// `audit`: print the recent safe-action audit rows.
fn cmd_audit(db_path: PathBuf, limit: u32) -> Result<()> {
    let store = Store::open(&db_path)?;
    let rows = store.recent_audit(limit)?;
    if rows.is_empty() {
        println!("No audit rows yet ({}).", db_path.display());
        return Ok(());
    }
    println!("{} recent audit row(s) (newest first):", rows.len());
    for a in rows {
        println!(
            "  [{}] {:<14} pid={:<6} {:<20} {:<16} {}",
            format_ts(a.ts_ms),
            a.action,
            a.pid,
            truncate(&a.image_name, 20),
            a.decision,
            a.detail
        );
    }
    Ok(())
}

/// `privacy`: print the current ConsentStore privacy-capability usage (M7).
/// Windows-only (registry read); a stub errors elsewhere.
#[cfg(windows)]
fn cmd_privacy() -> Result<()> {
    use atlas_collectors::{enumerate_privacy_usage, Capability};
    let usages = enumerate_privacy_usage(&[]);
    if usages.is_empty() {
        println!("No privacy-capability usage recorded in the ConsentStore.");
        return Ok(());
    }
    fn cap_label(c: Capability) -> &'static str {
        match c {
            Capability::Camera => "camera",
            Capability::Microphone => "microphone",
            Capability::Location => "location",
        }
    }
    println!("{} privacy usage row(s):", usages.len());
    println!(
        "{:<11} {:<6} {:<5} {:<40} {:<13} {:<13}",
        "CAPABILITY", "PKG", "USE", "APP", "LAST START", "LAST STOP"
    );
    for u in &usages {
        println!(
            "{:<11} {:<6} {:<5} {:<40} {:<13} {:<13}",
            cap_label(u.capability),
            if u.packaged { "pkg" } else { "desk" },
            if u.in_use { "yes" } else { "" },
            truncate(&u.display_name, 40),
            if u.last_start_ms == 0 {
                "-".to_string()
            } else {
                format_ts(u.last_start_ms)
            },
            if u.last_stop_ms == 0 {
                "-".to_string()
            } else {
                format_ts(u.last_stop_ms)
            },
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_privacy() -> Result<()> {
    anyhow::bail!("the `privacy` command requires Windows (ConsentStore registry)");
}

/// `startup`: print the startup inventory grouped by source (M7). Windows-only.
#[cfg(windows)]
fn cmd_startup() -> Result<()> {
    use atlas_collectors::{enumerate_startup, CollectorStartupSource};
    let entries = enumerate_startup();
    if entries.is_empty() {
        println!("No startup entries found.");
        return Ok(());
    }
    fn source_label(s: CollectorStartupSource) -> &'static str {
        match s {
            CollectorStartupSource::RunKeyMachine => "Run key (machine)",
            CollectorStartupSource::RunKeyUser => "Run key (user)",
            CollectorStartupSource::StartupFolderMachine => "Startup folder (machine)",
            CollectorStartupSource::StartupFolderUser => "Startup folder (user)",
            CollectorStartupSource::ScheduledTask => "Scheduled task",
            CollectorStartupSource::Service => "Service",
            CollectorStartupSource::PackagedTask => "Packaged task",
        }
    }
    // Group by source in the enum's declared order.
    let order = [
        CollectorStartupSource::RunKeyMachine,
        CollectorStartupSource::RunKeyUser,
        CollectorStartupSource::StartupFolderMachine,
        CollectorStartupSource::StartupFolderUser,
    ];
    println!("{} startup entry/entries:", entries.len());
    for src in order {
        let group: Vec<_> = entries.iter().filter(|e| e.source == src).collect();
        if group.is_empty() {
            continue;
        }
        println!("\n== {} ({}) ==", source_label(src), group.len());
        for e in group {
            println!(
                "  [{}] {:<28} {}",
                if e.enabled { "on " } else { "off" },
                truncate(&e.name, 28),
                truncate(&e.command, 80)
            );
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_startup() -> Result<()> {
    anyhow::bail!("the `startup` command requires Windows (registry/Startup folders)");
}

/// `services`: print the Win32 services inventory as a table (M7). Windows-only.
#[cfg(windows)]
fn cmd_services(filter: String) -> Result<()> {
    use atlas_collectors::{enumerate_services, CollectorServiceState, ServiceStartType};
    let services = enumerate_services(&filter);
    if services.is_empty() {
        if filter.is_empty() {
            println!("No services enumerated (unexpected — is the SCM reachable?).");
        } else {
            println!("No services match filter '{filter}'.");
        }
        return Ok(());
    }
    fn state_label(s: CollectorServiceState) -> &'static str {
        match s {
            CollectorServiceState::Stopped => "stopped",
            CollectorServiceState::StartPending => "start-pend",
            CollectorServiceState::StopPending => "stop-pend",
            CollectorServiceState::Running => "running",
            CollectorServiceState::ContinuePending => "cont-pend",
            CollectorServiceState::PausePending => "pause-pend",
            CollectorServiceState::Paused => "paused",
            CollectorServiceState::Unspecified => "?",
        }
    }
    fn start_label(s: ServiceStartType) -> &'static str {
        match s {
            ServiceStartType::Boot => "boot",
            ServiceStartType::System => "system",
            ServiceStartType::Auto => "auto",
            ServiceStartType::Manual => "manual",
            ServiceStartType::Disabled => "disabled",
            ServiceStartType::Unspecified => "?",
        }
    }
    println!(
        "{} service(s){}:",
        services.len(),
        if filter.is_empty() {
            String::new()
        } else {
            format!(" matching '{filter}'")
        }
    );
    println!(
        "{:<28} {:<11} {:<9} {:>7} {:<6} DISPLAY",
        "NAME", "STATE", "START", "PID", "DELAY"
    );
    for s in &services {
        println!(
            "{:<28} {:<11} {:<9} {:>7} {:<6} {}",
            truncate(&s.name, 28),
            state_label(s.state),
            start_label(s.start_type),
            if s.pid == 0 {
                "-".to_string()
            } else {
                s.pid.to_string()
            },
            if s.delayed_auto_start { "yes" } else { "" },
            truncate(&s.display_name, 40),
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_services(_filter: String) -> Result<()> {
    anyhow::bail!("the `services` command requires Windows (Service Control Manager)");
}

/// `action`: prepare (and optionally execute) a safe process action against the
/// in-process broker. Default is dry-run (Prepare only). This is Windows-only
/// (the broker uses Win32 process actions); a stub errors on other platforms.
#[cfg(windows)]
fn cmd_action(db_path: PathBuf, pid: u32, action: &str, yes: bool) -> Result<()> {
    use atlas_ipc::{ExecuteActionRequest, PrepareActionRequest, ProcessActionKind};

    let kind = match action.to_ascii_lowercase().as_str() {
        "close" | "close-windows" => ProcessActionKind::CloseWindows,
        "suspend" => ProcessActionKind::Suspend,
        "resume" => ProcessActionKind::Resume,
        "terminate" | "kill" => ProcessActionKind::Terminate,
        other => {
            anyhow::bail!("unknown action '{other}'. Use: suspend | resume | close | terminate")
        }
    };

    // Build a broker directly over the store — no pipe/serve needed for the dev
    // path. The audit log lands in the same db.
    let store = std::sync::Arc::new(std::sync::Mutex::new(Store::open(&db_path)?));
    let broker = broker::BrokerService::new(store);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        use atlas_ipc::AtlasControl;
        let prep = broker
            .prepare_action(tonic::Request::new(PrepareActionRequest {
                pid,
                create_time_100ns: 0,
                action: kind as i32,
            }))
            .await?
            .into_inner();

        println!("=== Prepare {action} on pid {pid} ===");
        if let Some(risk) = &prep.risk {
            println!(
                "risk: critical={} system={} visible_windows={} children={}",
                risk.is_critical, risk.is_system, risk.visible_windows, risk.child_count
            );
            for note in &risk.notes {
                println!("  note: {note}");
            }
        }
        if prep.allowed {
            println!(
                "verdict: ALLOWED (token issued, expires at {})",
                format_ts(prep.token_expires_ms)
            );
        } else {
            println!("verdict: DENIED — {}", prep.denial_reason);
        }

        if !yes {
            println!("(dry-run: pass --yes to execute; nothing was done)");
            return Ok::<(), anyhow::Error>(());
        }
        if !prep.allowed {
            println!("Not executing: prepare was denied.");
            return Ok(());
        }

        let exec = broker
            .execute_action(tonic::Request::new(ExecuteActionRequest {
                consent_token: prep.consent_token,
            }))
            .await?
            .into_inner();
        println!(
            "=== Execute === success={} — {}",
            exec.success, exec.message
        );
        Ok(())
    })?;
    Ok(())
}

#[cfg(not(windows))]
fn cmd_action(_db_path: PathBuf, _pid: u32, _action: &str, _yes: bool) -> Result<()> {
    anyhow::bail!("the `action` command requires Windows process-action APIs");
}

fn cmd_db_top(db_path: PathBuf, minutes: u64, limit: u32) -> Result<()> {
    let store = Store::open(&db_path)?;
    let now = now_ms();
    let since = now - (minutes as i64) * 60_000;
    let rows = store.top_processes(since, now, limit)?;
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
        "Top processes by average CPU over the last {minutes} minutes ({} rows, source: {})",
        rows.len(),
        db_path.display()
    );
    println!(
        "{:>7} {:<30} {:>8} {:>9} {:>11} {:>8}",
        "PID", "NAME", "AVG CPU%", "PEAK CPU%", "PEAK WS MB", "SAMPLES"
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

/// Best-effort read of total physical memory (bytes) for the memory-pressure
/// percent-of-total threshold. Takes one live sample; returns 0 if unavailable
/// (memory detection/diagnosis then degrades to CPU only, never fabricates).
#[cfg(windows)]
fn current_mem_total() -> u64 {
    match Sampler::new().and_then(|mut s| s.sample()) {
        Ok(set) => set.system.mem_total,
        Err(_) => 0,
    }
}

#[cfg(not(windows))]
fn current_mem_total() -> u64 {
    0
}

/// Human label for an incident kind discriminant (dev display).
fn incident_kind_label(kind: i32) -> &'static str {
    match kind {
        detectors::KIND_CPU_SATURATION => "CPU saturation",
        detectors::KIND_MEMORY_PRESSURE => "Memory pressure",
        detectors::KIND_DISK_LATENCY => "Disk latency",
        _ => "unspecified",
    }
}

/// Human label for a severity discriminant (dev display).
fn severity_label(sev: i32) -> &'static str {
    match sev {
        detectors::SEV_INFO => "info",
        detectors::SEV_WARNING => "warning",
        detectors::SEV_CRITICAL => "critical",
        _ => "?",
    }
}

/// Converts a store incident row to the proto `Incident` (0 end = ongoing).
fn incident_row_to_proto(r: &atlas_store::IncidentRow) -> atlas_ipc::Incident {
    atlas_ipc::Incident {
        id: r.id,
        kind: r.kind,
        start_ms: r.start_ms,
        end_ms: r.end_ms.unwrap_or(0),
        severity: r.severity,
        peak_value: r.peak_value,
        summary: r.summary.clone(),
    }
}

/// `incidents`: refresh detection over the window (idempotent) then list.
fn cmd_incidents(db_path: PathBuf, minutes: u64, limit: u32) -> Result<()> {
    let store = Store::open(&db_path)?;
    let now = now_ms();
    let from = now - (minutes as i64) * 60_000;
    let mem_total = current_mem_total();
    // Refresh: catch any incidents in this window not already persisted (e.g.
    // data recorded before detection existed). Idempotent by (kind, start).
    let found = detectors::run_detection_pass(&store, from, now, mem_total)?;
    let (rows, truncated) = store.list_incidents(from, now, limit)?;
    println!(
        "Incidents over the last {minutes} min ({} shown{}, {} upserted this pass, source {})",
        rows.len(),
        if truncated { ", truncated" } else { "" },
        found,
        db_path.display()
    );
    if mem_total == 0 {
        println!("(note: total memory unknown here — memory-pressure detection skipped)");
    }
    if rows.is_empty() {
        println!("(no incidents — record under load, or widen --minutes)");
        return Ok(());
    }
    println!(
        "{:>5} {:<16} {:<9} {:<13} {:<13} {:>6}  SUMMARY",
        "ID", "KIND", "SEVERITY", "START", "END", "PEAK%"
    );
    for r in &rows {
        println!(
            "{:>5} {:<16} {:<9} {:<13} {:<13} {:>6.0}  {}",
            r.id,
            incident_kind_label(r.kind),
            severity_label(r.severity),
            format_ts(r.start_ms),
            r.end_ms.map(format_ts).unwrap_or_else(|| "ongoing".into()),
            r.peak_value,
            truncate(&r.summary, 60),
        );
    }
    Ok(())
}

/// Resolves an incident id (or an ad-hoc `minutes` range) and diagnoses it,
/// returning the proto incident + the diagnose reply.
fn resolve_and_diagnose(
    store: &Store,
    incident: Option<i64>,
    minutes: Option<u64>,
    now: i64,
    mem_total: u64,
) -> Result<(atlas_ipc::Incident, atlas_ipc::DiagnoseReply)> {
    match incident {
        Some(id) => {
            let row = store
                .get_incident(id)?
                .ok_or_else(|| anyhow::anyhow!("no incident #{id} (run `incidents` first)"))?;
            let ctx = diagnostics::DiagnoseContext {
                kind: row.kind,
                start_ms: row.start_ms,
                end_ms: row.end_ms.unwrap_or(0),
                peak_value: row.peak_value,
            };
            let reply = diagnostics::diagnose(store, &ctx, now, mem_total)?;
            Ok((incident_row_to_proto(&row), reply))
        }
        None => {
            let mins = minutes.unwrap_or(10);
            let from = now - (mins as i64) * 60_000;
            let ctx = diagnostics::DiagnoseContext {
                kind: 0, // inferred from the data
                start_ms: from,
                end_ms: 0,
                peak_value: 0.0,
            };
            let reply = diagnostics::diagnose(store, &ctx, now, mem_total)?;
            let inc = atlas_ipc::Incident {
                id: 0,
                kind: 0,
                start_ms: from,
                end_ms: 0,
                severity: 0,
                peak_value: 0.0,
                summary: format!("Ad-hoc diagnosis of the last {mins} min"),
            };
            Ok((inc, reply))
        }
    }
}

/// `diagnose`: print the structured diagnosis (as a plain-text report).
fn cmd_diagnose(db_path: PathBuf, incident: Option<i64>, minutes: Option<u64>) -> Result<()> {
    let store = Store::open(&db_path)?;
    let now = now_ms();
    let mem_total = current_mem_total();
    let (inc, reply) = resolve_and_diagnose(&store, incident, minutes, now, mem_total)?;
    if !reply.available {
        println!("Diagnosis unavailable: {}", reply.unavailable_reason);
        return Ok(());
    }
    // No redaction for the local dev view.
    let (content, _ct) = report::render_report(
        &inc,
        &reply,
        atlas_ipc::ReportFormat::ReportText,
        &atlas_ipc::RedactionOptions::default(),
    );
    print!("{content}");
    Ok(())
}

/// Parses a report-format token.
fn parse_report_format(token: &str) -> Result<atlas_ipc::ReportFormat> {
    Ok(match token.to_ascii_lowercase().as_str() {
        "text" | "txt" => atlas_ipc::ReportFormat::ReportText,
        "json" => atlas_ipc::ReportFormat::ReportJson,
        "csv" => atlas_ipc::ReportFormat::ReportCsv,
        "html" => atlas_ipc::ReportFormat::ReportHtml,
        other => anyhow::bail!("unknown format '{other}'. Use: text | json | csv | html"),
    })
}

/// `report`: render a diagnosis report in the chosen format, with redaction.
fn cmd_report(
    db_path: PathBuf,
    incident: Option<i64>,
    minutes: Option<u64>,
    format: &str,
    out: Option<PathBuf>,
    redaction: atlas_ipc::RedactionOptions,
) -> Result<()> {
    let fmt = parse_report_format(format)?;
    let store = Store::open(&db_path)?;
    let now = now_ms();
    let mem_total = current_mem_total();
    let (inc, reply) = resolve_and_diagnose(&store, incident, minutes, now, mem_total)?;
    let (content, content_type) = report::render_report(&inc, &reply, fmt, &redaction);
    match out {
        Some(path) => {
            std::fs::write(&path, content.as_bytes())?;
            println!(
                "Wrote {} report ({}) to {}",
                format,
                content_type,
                path.display()
            );
        }
        None => print!("{content}"),
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

    fn sys_metrics() -> SysMetrics {
        SysMetrics {
            cpu_permille: 100,
            mem_used: 1 << 30,
            mem_total: 8 << 30,
            commit_used: 2 << 30,
            process_count: 200,
            thread_count: 2000,
            handle_count: 40000,
        }
    }

    fn proc_metrics(cpu: u32) -> ProcMetrics {
        ProcMetrics {
            cpu_permille: cpu,
            working_set: 100 << 20,
            private_bytes: 80 << 20,
            read_bps: 0,
            write_bps: 0,
        }
    }

    /// A head that reaches the point cap seals into exactly the six system
    /// series (one block each) and clears.
    #[test]
    fn block_writer_seals_sys_series_on_point_cap() {
        let mut bw = BlockWriter::new();
        for i in 0..SEAL_MAX_POINTS as i64 {
            bw.append_sys(1000 + i * 1000, &sys_metrics());
        }
        let blocks = bw.drain_sealed();
        // Six Sys* series, each sealed once at the cap.
        assert_eq!(blocks.len(), 6);
        assert!(blocks.iter().all(|b| b.points == SEAL_MAX_POINTS));
        assert!(blocks.iter().all(|b| b.key.scope == SYSTEM_SCOPE));
    }

    /// Draining a process scope on exit flushes its five series and forgets it,
    /// so the cardinality guard no longer tracks it.
    #[test]
    fn block_writer_drains_scope_on_exit() {
        let mut bw = BlockWriter::new();
        bw.append_proc(1000, 42, &proc_metrics(500));
        bw.append_proc(2000, 42, &proc_metrics(400));
        let blocks = bw.drain_scope(42);
        assert_eq!(blocks.len(), 5, "five per-process series");
        assert!(blocks.iter().all(|b| b.key.scope == 42 && b.points == 2));
        assert!(!bw.scope_last_seen.contains_key(&42));
    }

    /// The cardinality guard seals+forgets a scope idle past the horizon while
    /// leaving a recently-seen scope open.
    #[test]
    fn block_writer_evicts_idle_scope() {
        let mut bw = BlockWriter::new();
        bw.append_proc(1000, 1, &proc_metrics(10)); // last seen at t=1000
        bw.append_proc(1000, 2, &proc_metrics(20));
        // Scope 2 keeps getting samples; scope 1 goes quiet.
        let now = 1000 + SCOPE_IDLE_EVICT_MS;
        bw.append_proc(now, 2, &proc_metrics(20));

        let evicted = bw.evict_idle(now);
        // Only scope 1's five series are shed.
        assert_eq!(evicted.len(), 5);
        assert!(evicted.iter().all(|b| b.key.scope == 1));
        assert!(!bw.scope_last_seen.contains_key(&1));
        assert!(bw.scope_last_seen.contains_key(&2), "active scope kept");
    }

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

    #[cfg(windows)]
    #[test]
    fn format_event_image_load() {
        use atlas_collectors::{ProcessEvent, ProcessEventKind};
        let ev = ProcessEvent {
            ts_ms: (3600 + 2 * 60 + 3) * 1000 + 4,
            pid: 42,
            kind: ProcessEventKind::ImageLoaded {
                image_base: 0x1000,
                image_size: 0x2000,
                image_name: r"\Device\HarddiskVolume4\ntdll.dll".into(),
            },
        };
        assert_eq!(
            format_event(&ev),
            r"[01:02:03.004] IMAGE pid=42 base=0x1000 size=8192 \Device\HarddiskVolume4\ntdll.dll"
        );
    }

    /// A Start event folds into a start count and a `proc_event` start row; a
    /// Stop folds into an exit count, an exit stamp, and a stop row. Image loads
    /// (which `record` never enables) are ignored if one slips through.
    #[cfg(windows)]
    #[test]
    fn fold_event_routes_start_stop_and_ignores_images() {
        use atlas_collectors::{ProcessEvent, ProcessEventKind};
        let mut win = EventWindow::default();

        fold_event(
            &mut win,
            ProcessEvent {
                ts_ms: 1_000,
                pid: 7,
                kind: ProcessEventKind::Started {
                    parent_pid: 4,
                    session_id: 1,
                    image_name: "child.exe".into(),
                },
            },
        );
        fold_event(
            &mut win,
            ProcessEvent {
                ts_ms: 2_000,
                pid: 7,
                kind: ProcessEventKind::Stopped { exit_status: 3 },
            },
        );
        fold_event(
            &mut win,
            ProcessEvent {
                ts_ms: 2_500,
                pid: 9,
                kind: ProcessEventKind::ImageLoaded {
                    image_base: 1,
                    image_size: 2,
                    image_name: "ntdll.dll".into(),
                },
            },
        );

        assert_eq!(win.started, 1);
        assert_eq!(win.exited, 1);
        assert_eq!(win.rows.len(), 2, "image load produced no row");
        assert_eq!(win.rows[0].kind, PROC_EVENT_START);
        assert_eq!(win.rows[0].image_name.as_deref(), Some("child.exe"));
        assert_eq!(win.rows[1].kind, PROC_EVENT_STOP);
        assert_eq!(win.rows[1].exit_status, Some(3));
        assert_eq!(win.exit_stamps, vec![(7, 2_000, Some(3))]);

        // take() hands off buffers and resets the counters for the next window.
        let (rows, stamps) = win.take();
        assert_eq!(rows.len(), 2);
        assert_eq!(stamps.len(), 1);
        assert_eq!(win.started, 0);
        assert_eq!(win.exited, 0);
        assert!(win.rows.is_empty());
    }
}
