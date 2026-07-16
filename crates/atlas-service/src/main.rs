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
mod dynamic_protection;
#[cfg(windows)]
mod forensics;
#[cfg(windows)]
mod ipc;
#[cfg(windows)]
mod plugins;
mod privacy_alerts;
mod report;
mod rules;
#[cfg(windows)]
mod rules_service;
mod service_ctl;
mod soak;
mod support_bundle;

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
        /// Stop cleanly after this many seconds (default: run until Ctrl+C). A
        /// clean stop runs the rules engine's shutdown restore, so it is the
        /// verification path for reversibility.
        #[arg(long)]
        duration: Option<u64>,
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
    /// Report per-tier sample-block storage + a simulated footprint comparison
    /// (R3 extended retention tiers). Shows block counts + bytes for T0/T1/T2 and
    /// a tiered-vs-raw-only retained-footprint estimate over a synthetic series.
    ///
    /// With `--rollup`, force a compaction pass first using tiny retentions so a
    /// short recording visibly demotes T0 blocks into T1/T2 (e.g.
    /// `storage --rollup --raw-retention-secs 5`).
    Storage {
        /// Database path (default: %LOCALAPPDATA%\SystemAtlas\dev\atlas.db).
        #[arg(long)]
        db: Option<PathBuf>,
        /// Force a roll-up + retention pass before reporting.
        #[arg(long)]
        rollup: bool,
        /// T0 (raw) retention seconds for the forced pass — samples older than
        /// this roll into T1 (default 5, so a ~20 s recording visibly demotes).
        #[arg(long, default_value_t = 5)]
        raw_retention_secs: u64,
        /// T1 retention seconds for the forced pass — older T1 rolls into T2.
        #[arg(long, default_value_t = 3600)]
        t1_retention_secs: u64,
        /// Days of synthetic 1 s history for the footprint simulation.
        #[arg(long, default_value_t = 30)]
        sim_days: u64,
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
    /// Deep-inspect a process by pid (R2): identity + optional sections.
    ///
    /// Prints the full process detail (path, command line, user, integrity,
    /// architecture, signature, versions — PRD §9.4). Add `--handles`,
    /// `--modules`, and/or `--threads` for those on-demand sections. Runs
    /// unprivileged; cross-user/protected fields degrade with `limited`/
    /// `names_limited` rather than failing. Inspect your own service pid or a
    /// notepad you spawned.
    Inspect {
        /// Target process id.
        #[arg(long)]
        pid: u32,
        /// Also list the process's open handles (types + resolved names).
        #[arg(long)]
        handles: bool,
        /// Also list the process's loaded modules (version + signed).
        #[arg(long)]
        modules: bool,
        /// Also list the process's threads (state, start address, times).
        #[arg(long)]
        threads: bool,
        /// Cap on handle rows (0 = default).
        #[arg(long, default_value_t = 0)]
        handle_limit: u32,
    },
    /// Deep security detail for a process by pid (R3, PRD §9.4.1/§9.4.6).
    ///
    /// Prints the on-disk image SHA-256, the signature status + signing
    /// certificate chain (leaf → root), the token privileges/groups/capabilities,
    /// and the readable process mitigation policies. Runs unprivileged; cross-
    /// user/protected fields degrade with `limited` rather than failing. Inspect
    /// your own service pid, or a signed process (e.g. a .NET host) for a chain.
    Security {
        /// Target process id.
        #[arg(long)]
        pid: u32,
    },
    /// Find what is using a file or folder (R2, PRD §9.5).
    ///
    /// Restart-Manager resource-ownership search: prints the processes/services
    /// holding `path`. Unprivileged for user-accessible files. To verify, open a
    /// file in one shell and `locks <that path>` from another.
    Locks {
        /// The file or folder path to look up.
        path: String,
    },
    /// List TCP/UDP connections with owning process + DNS-cache domains (R2).
    ///
    /// iphlpapi owner-pid tables (PRD §9.12) — the same data `ListConnections`
    /// serves. Unprivileged. `--listening` also includes TCP LISTEN rows and UDP
    /// binds (which have no remote endpoint).
    Connections {
        /// Also include listening TCP + bound UDP sockets.
        #[arg(long)]
        listening: bool,
    },
    /// List listening TCP + bound UDP ports with owning process (R2, PRD §9.12).
    ///
    /// The same data `ListListeningPorts` serves. Unprivileged.
    Ports,
    /// List scheduled tasks via the Task Scheduler COM API (R2, PRD §9.9.2).
    ///
    /// The same data `ListScheduledTasks` serves. Unprivileged; cross-user task
    /// state may be limited without elevation. `--filter` is a case-insensitive
    /// substring over name/path.
    Tasks {
        /// Case-insensitive substring over name/path (empty = all).
        #[arg(long)]
        filter: Option<String>,
    },
    /// Report boot performance from the Diagnostics-Performance log (R2, §9.8.4).
    ///
    /// The same data `ListBoots` serves. The channel is often readable only when
    /// elevated; a clear unavailable message prints otherwise.
    Boots {
        /// Max boot records to show (0 = default).
        #[arg(long, default_value_t = 0)]
        limit: u32,
    },
    /// Report battery status + health (R2, PRD §9.6.6).
    ///
    /// The same data `GetBatteryStatus` serves. On a desktop prints
    /// "no battery present". Unprivileged.
    Battery,
    /// Report ACPI thermal-zone temperatures via WMI (R2, PRD §9.6.7).
    ///
    /// The same data `GetThermal` serves. Prints an honest unavailable message
    /// when no thermal sensor is exposed. Unprivileged.
    Thermal,
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
    /// Assemble a redacted remote support bundle (R3, PRD §9.18): one self-
    /// contained diagnostic document (health, incidents+diagnoses, changes,
    /// crashes, inventories, own overhead) in html | json | text.
    ///
    /// Every textual field runs through the shared redactor before formatting,
    /// so a bundle never leaks more than a single report. This is the backend
    /// verification path — no `serve` needed.
    SupportBundle {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Output format: html | json | text.
        #[arg(long, default_value = "html")]
        format: String,
        /// Window to summarize (incidents/changes/crashes), in minutes.
        #[arg(long, default_value_t = 4320)]
        minutes: u64,
        /// Comma-separated sections (empty = all): device, health, incidents,
        /// changes, crashes, services, startup, self-metrics.
        #[arg(long)]
        sections: Option<String>,
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Replace file paths with <PATH>.
        #[arg(long)]
        redact_paths: bool,
        /// Replace the current user name with <USER>.
        #[arg(long)]
        redact_users: bool,
        /// Replace the computer name with <HOST>.
        #[arg(long)]
        redact_host: bool,
        /// Replace command-line arguments with <CMD-ARGS>.
        #[arg(long)]
        redact_cmdlines: bool,
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
    /// Manage performance rules (R2, PRD §9.7): add/list/enable/disable/rm/simulate.
    ///
    /// Rules persist in the store; a running `serve` applies enabled rules on
    /// each ~1 s tick and reverses them when disabled/removed. `simulate` is a
    /// pure dry-run against the current snapshot (applies nothing) and needs no
    /// `serve`.
    Rule {
        #[arg(long)]
        db: Option<PathBuf>,
        #[command(subcommand)]
        cmd: RuleCmd,
    },
    /// Manage signed plugins (R3, PRD §18.3): register/list/enable/disable/grant/
    /// rm/launch. Out-of-process, Authenticode-signed, capability-scoped READ-ONLY
    /// extensions. `register` verifies the executable's signature and REFUSES an
    /// unsigned one unless `--allow-unsigned`. Registry ops act on the store
    /// directly (no `serve` needed); `launch` mints a one-time nonce and runs the
    /// bundled example plugin against a running `serve`.
    Plugin {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Pipe discriminator for `launch` (must match the server's `serve --pipe`).
        #[arg(long)]
        pipe: Option<String>,
        #[command(subcommand)]
        cmd: PluginCmd,
    },
    /// List the live interventions a running `serve` is currently applying (R2).
    ///
    /// Connects to `serve` over the pipe and calls ListInterventions — the
    /// in-memory reversal ledger only exists inside the running service.
    Interventions {
        /// Pipe discriminator; must match the server's `serve --pipe`.
        #[arg(long)]
        pipe: Option<String>,
    },
    /// Manage rule profiles (R2, PRD §9.7.4): add/list/activate/deactivate.
    ///
    /// A profile is a named bundle of rules plus a power mode. Activating enables
    /// its rules (and deactivates other profiles' exclusive rules); a running
    /// `serve` applies the resulting enabled-set on its next tick.
    Profile {
        #[arg(long)]
        db: Option<PathBuf>,
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
    /// Print a process's current priority / affinity / EcoQoS + trigger inputs
    /// (R2 verification helper). Reads via the collector policy FFI; unprivileged
    /// for same-user targets, degrades on cross-user/protected ones.
    Policy {
        /// Target process id.
        #[arg(long)]
        pid: u32,
    },
    /// Show or set the dynamic responsiveness protection config on a running
    /// `serve` (R3, PRD §9.7.3). Talks to the service over the pipe so changes
    /// take effect live: enabling starts the watchdog, disabling restores every
    /// active dampening at once. Use `interventions` to see active dampenings.
    DynamicProtection {
        /// Pipe discriminator; must match the server's `serve --pipe`.
        #[arg(long)]
        pipe: Option<String>,
        #[command(subcommand)]
        cmd: DynProtCmd,
    },
    /// Manage advanced privacy-alert rules (R2, PRD §9.10.3): add/list/rm.
    ///
    /// Rules persist in the store; a running `serve` evaluates them against
    /// ConsentStore camera/mic/location transitions and records fired alerts.
    PrivacyAlert {
        #[arg(long)]
        db: Option<PathBuf>,
        #[command(subcommand)]
        cmd: PrivacyAlertCmd,
    },
    /// Print recorded fired privacy alerts (R2, PRD §9.10.3).
    ///
    /// Reads the store's `fired_alert` table — the alerts a running `serve`'s
    /// evaluator recorded. The same data `ListFiredAlerts` serves.
    FiredAlerts {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Look-back window in minutes (default: 24 h).
        #[arg(long, default_value_t = 1440)]
        minutes: u64,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Watch live privacy-capability transitions from the ConsentStore (R2).
    ///
    /// Arms the `RegNotifyChangeKeyValue` change-watcher and prints each
    /// camera/mic/location start/stop (with foreground + session-locked hints)
    /// until Ctrl+C. The direct verification path for the watcher — no `serve`
    /// needed. Trigger one by starting/stopping a mic or camera app.
    PrivacyWatch,
    /// Print recorded system changes (R3, PRD §9.13).
    ///
    /// Reads the store's `system_change` table — the changes a running `serve`'s
    /// change detector recorded. The same data `ListSystemChanges` serves.
    Changes {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Look-back window in minutes (default: 7 days).
        #[arg(long, default_value_t = 10080)]
        minutes: u64,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Print recorded crashes with their correlation context (R3, PRD §9.14).
    ///
    /// Reads the store's `crash_record` table — the crashes a running `serve`'s
    /// crash scanner read + correlated. The same data `ListCrashes` serves.
    Crashes {
        #[arg(long)]
        db: Option<PathBuf>,
        /// Look-back window in minutes (default: 7 days).
        #[arg(long, default_value_t = 10080)]
        minutes: u64,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// One-shot: seed + diff the inventory against the stored baseline (R3).
    ///
    /// Runs one change-detection pass directly (no `serve`): collects the current
    /// app/service/startup/task/power/default-app inventory, diffs it against the
    /// persisted baseline (recording any differences), imports WUA update history,
    /// then rewrites the baseline. Run once to seed, again to see changes.
    DetectChanges {
        #[arg(long)]
        db: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum PrivacyAlertCmd {
    /// Add an alert rule (enabled unless --disabled).
    Add {
        /// Capability: camera | microphone | location | all (default all).
        #[arg(long)]
        capability: Option<String>,
        /// Condition: any-use | background | while-locked | unknown-app |
        /// longer-than (longer-than needs --threshold).
        #[arg(long)]
        condition: String,
        /// Threshold in seconds for `longer-than`.
        #[arg(long)]
        threshold: Option<u32>,
        /// Friendly rule name (defaults to "<capability> <condition>").
        #[arg(long)]
        name: Option<String>,
        /// Create the rule disabled.
        #[arg(long)]
        disabled: bool,
    },
    /// List all alert rules.
    List,
    /// Delete an alert rule by id.
    Rm { id: i64 },
}

/// Shared rule-authoring arguments for `rule add` and `rule simulate`. All
/// fields are optional at parse time (so `simulate --id N` needs no `--match`);
/// `--match` is validated as required at runtime for authoring.
#[derive(clap::Args, Clone)]
struct RuleArgs {
    /// Case-insensitive image name to match, e.g. `chrome.exe` (required for
    /// `add`, and for `simulate` without `--id`).
    #[arg(long = "match")]
    match_image: Option<String>,
    /// Friendly rule name (defaults to the match image).
    #[arg(long)]
    name: Option<String>,
    /// Priority class: idle | below-normal | normal | above-normal | high.
    #[arg(long)]
    priority: Option<String>,
    /// Affinity mode: all | prefer-p | prefer-e | custom (custom needs --mask).
    #[arg(long)]
    affinity: Option<String>,
    /// Custom affinity bitmask (hex, e.g. 0xF) when --affinity custom.
    #[arg(long)]
    mask: Option<String>,
    /// Enable EcoQoS (efficiency mode) on matching processes.
    #[arg(long)]
    eco: bool,
    /// Trigger: while-running | ac | dc | fullscreen | gpu-load | gpu-thermal.
    #[arg(long)]
    trigger: Option<String>,
    /// Precedence — higher wins on conflict (default 0).
    #[arg(long, default_value_t = 0)]
    precedence: i32,
}

#[derive(Subcommand)]
enum RuleCmd {
    /// Add a rule (enabled unless --disabled).
    Add {
        #[command(flatten)]
        args: RuleArgs,
        /// Create the rule disabled.
        #[arg(long)]
        disabled: bool,
    },
    /// List all rules.
    List,
    /// Enable a rule by id.
    Enable { id: i64 },
    /// Disable a rule by id.
    Disable { id: i64 },
    /// Delete a rule by id.
    Rm { id: i64 },
    /// Dry-run a rule against the current snapshot (applies nothing). Either an
    /// existing `--id`, or the same authoring flags as `add`.
    Simulate {
        /// Simulate an existing saved rule by id.
        #[arg(long)]
        id: Option<i64>,
        #[command(flatten)]
        args: Option<RuleArgs>,
    },
}

#[derive(Subcommand)]
enum PluginCmd {
    /// Register a plugin executable after verifying its Authenticode signature.
    /// Unsigned executables are REFUSED unless `--allow-unsigned`.
    Register {
        /// Path to the plugin executable.
        exe: String,
        /// Capabilities to grant, comma-separated: snapshot,history,search,
        /// incidents,inventory,network,forensics.
        #[arg(long, default_value = "")]
        caps: String,
        /// Explicit, unsafe opt-in to register an unsigned executable.
        #[arg(long)]
        allow_unsigned: bool,
    },
    /// List registered plugins.
    List,
    /// Enable a plugin by id (re-verifies the signature; refuses if degraded).
    Enable { id: i64 },
    /// Disable a plugin by id.
    Disable { id: i64 },
    /// Replace a plugin's granted capabilities (comma-separated, see `register`).
    Grant {
        id: i64,
        #[arg(long, default_value = "")]
        caps: String,
    },
    /// Remove a plugin by id.
    Rm { id: i64 },
    /// Mint a one-time launch nonce for a plugin and run the bundled example
    /// plugin against a running `serve`, or (with `--print-nonce`) just print the
    /// nonce for a manually launched plugin to read.
    Launch {
        id: i64,
        /// Print the minted nonce instead of spawning the example plugin (for a
        /// plugin you launch yourself; it reads ATLAS_PLUGIN_NONCE).
        #[arg(long)]
        print_nonce: bool,
    },
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// Add a profile bundling the given rule ids.
    Add {
        /// Profile name.
        name: String,
        /// Rule id to include (repeatable).
        #[arg(long = "rule")]
        rules: Vec<i64>,
        /// Power mode: "" | PowerSaver | Balanced | HighPerformance.
        #[arg(long, default_value = "")]
        power_mode: String,
    },
    /// List all profiles.
    List,
    /// Activate a profile by id (enables its rules; applies its power mode).
    Activate { id: i64 },
    /// Deactivate a profile by id (disables its rules).
    Deactivate { id: i64 },
}

#[derive(Subcommand)]
enum DynProtCmd {
    /// Print the current dynamic-protection config.
    Show,
    /// Update the config (validated + persisted + applied live). Omitting
    /// `--enabled` disables the watchdog (and restores all active dampenings).
    Set {
        /// Enable the watchdog (omit to disable it).
        #[arg(long)]
        enabled: bool,
        /// System-CPU share threshold, permille (1..=1000). 800 = 80% of total.
        #[arg(long, default_value_t = 800)]
        threshold: u32,
        /// Seconds a process must stay above threshold before intervening.
        #[arg(long, default_value_t = 30)]
        sustain: u32,
        /// Hard auto-restore cap: max seconds a dampening may ever be held.
        #[arg(long, default_value_t = 300)]
        max: u32,
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
    // clap's debug-build argument parser, combined with the large `Cmd` enum,
    // needs more than the ~1 MB default Windows main-thread stack — even a fresh
    // `--help` overflows it in a debug build. Run the whole CLI on a worker thread
    // with a roomy stack; the `serve`/`record` paths spawn their own threads and
    // tokio runtimes underneath this one, unaffected.
    std::thread::Builder::new()
        .name("atlas-cli".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(run)?
        .join()
        .map_err(|_| anyhow::anyhow!("atlas-service CLI thread panicked"))?
}

fn run() -> Result<()> {
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
        Cmd::Storage {
            db,
            rollup,
            raw_retention_secs,
            t1_retention_secs,
            sim_days,
        } => cmd_storage(
            db.unwrap_or_else(default_db_path),
            rollup,
            raw_retention_secs,
            t1_retention_secs,
            sim_days,
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
        Cmd::Inspect {
            pid,
            handles,
            modules,
            threads,
            handle_limit,
        } => cmd_inspect(pid, handles, modules, threads, handle_limit),
        Cmd::Security { pid } => cmd_security(pid),
        Cmd::Locks { path } => cmd_locks(&path),
        Cmd::Connections { listening } => cmd_connections(listening),
        Cmd::Ports => cmd_ports(),
        Cmd::Tasks { filter } => cmd_tasks(filter.unwrap_or_default()),
        Cmd::Boots { limit } => cmd_boots(limit),
        Cmd::Battery => cmd_battery(),
        Cmd::Thermal => cmd_thermal(),
        Cmd::Serve { pipe, db, duration } => {
            cmd_serve(pipe, db.unwrap_or_else(default_db_path), duration)
        }
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
        Cmd::Rule { db, cmd } => cmd_rule(db.unwrap_or_else(default_db_path), cmd),
        Cmd::Plugin { db, pipe, cmd } => cmd_plugin(db.unwrap_or_else(default_db_path), pipe, cmd),
        Cmd::Interventions { pipe } => cmd_interventions(pipe),
        Cmd::Profile { db, cmd } => cmd_profile(db.unwrap_or_else(default_db_path), cmd),
        Cmd::Policy { pid } => cmd_policy(pid),
        Cmd::DynamicProtection { pipe, cmd } => cmd_dynamic_protection(pipe, cmd),
        Cmd::PrivacyAlert { db, cmd } => cmd_privacy_alert(db.unwrap_or_else(default_db_path), cmd),
        Cmd::FiredAlerts { db, minutes, limit } => {
            cmd_fired_alerts(db.unwrap_or_else(default_db_path), minutes, limit)
        }
        Cmd::PrivacyWatch => cmd_privacy_watch(),
        Cmd::Changes { db, minutes, limit } => {
            cmd_changes(db.unwrap_or_else(default_db_path), minutes, limit)
        }
        Cmd::Crashes { db, minutes, limit } => {
            cmd_crashes(db.unwrap_or_else(default_db_path), minutes, limit)
        }
        Cmd::DetectChanges { db } => cmd_detect_changes(db.unwrap_or_else(default_db_path)),
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
        Cmd::SupportBundle {
            db,
            format,
            minutes,
            sections,
            out,
            redact_paths,
            redact_users,
            redact_host,
            redact_cmdlines,
        } => cmd_support_bundle(
            db.unwrap_or_else(default_db_path),
            &format,
            minutes,
            sections.as_deref().unwrap_or(""),
            out,
            atlas_ipc::RedactionOptions {
                redact_user_names: redact_users,
                redact_computer_name: redact_host,
                redact_paths,
                redact_command_lines: redact_cmdlines,
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
    gpu_permille: u32,
    gpu_dedicated_bytes: u64,
    gpu_shared_bytes: u64,
}

impl ProcMetrics {
    fn from_sample(p: &ProcSample) -> Self {
        Self {
            cpu_permille: p.cpu_permille,
            working_set: p.working_set,
            private_bytes: p.private_bytes,
            read_bps: p.read_bps,
            write_bps: p.write_bps,
            gpu_permille: p.gpu_permille,
            gpu_dedicated_bytes: p.gpu_dedicated_bytes,
            gpu_shared_bytes: p.gpu_shared_bytes,
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
    gpu_permille: u32,
    gpu_dedicated_used: u64,
    gpu_shared_used: u64,
    gpu_memory_budget: u64,
    gpu_throttling: Option<bool>,
}

/// One tick's worth of raw samples handed to the writer: the timestamp, the
/// system gauges, and every process seen with its identity (so the writer can
/// resolve the scope/row-id) and its five metric values.
struct TickSamples {
    ts_ms: i64,
    sys: SysMetrics,
    procs: Vec<(ProcIdentity, ProcMetrics)>,
    gpu_adapters: Vec<atlas_collectors::GpuAdapterSample>,
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

// ---------------------------------------------------------------------------
// R3 extended retention tiers (PRD §9.3.1/§13.5, tech-stack §4.2).
//
// The compaction job demotes aged raw (T0) samples into coarser roll-up tiers so
// 30–90 days of history stay at a bounded FOOTPRINT while peaks survive (min/max
// stored explicitly). Note the honest scope: tiering bounds the *retained
// footprint* over long windows — it does NOT change the ~MB/day WRITE rate,
// which is governed by adaptive cadence + batching, not by roll-up. The
// compaction pass runs idle-only on the writer thread (and once at startup), so
// it never fights the 1 Hz sampling path.
// ---------------------------------------------------------------------------

/// T0 raw retention (default 72 h) — raw 1 s samples older than this are rolled
/// into T1 and then dropped.
const RAW_RETENTION_MS: i64 = RETENTION_HOURS * 3_600_000;
/// T1 (10 s roll-up) retention (default 14 d) — older T1 is rolled into T2.
const T1_RETENTION_MS: i64 = 14 * 24 * 3_600_000;
/// T2 (60 s roll-up) retention (default 90 d) — the long tail; dropped past this.
const T2_RETENTION_MS: i64 = 90 * 24 * 3_600_000;
/// Margin around a pinned incident/bookmark within which blocks are never
/// demoted or deleted (they keep full 1 s resolution).
const PIN_MARGIN_MS: i64 = 60_000;
/// Idle cadence for the compaction pass on the writer thread (5 min). Cheap and
/// idle-only — it runs between flush windows, never on the sampling loop.
const COMPACT_EVERY_MS: u128 = 5 * 60_000;

/// One compaction pass: roll T0→T1 (blocks older than `raw_ret_ms`), then
/// T1→T2 (older than `t1_ret_ms`), then apply per-tier retention deletion —
/// all pin-aware, so bookmarked incident windows are never downsampled or swept
/// (tech-stack §4.2). Best-effort: returns the roll-up/retention tallies for
/// logging. `now` is wall-clock ms so callers can pass a controlled clock.
fn run_compaction(
    store: &mut Store,
    now: i64,
    raw_ret_ms: i64,
    t1_ret_ms: i64,
    t2_ret_ms: i64,
) -> Result<()> {
    let s1 = store.rollup_tier(atlas_tsdb::TIER_RAW, now - raw_ret_ms, PIN_MARGIN_MS)?;
    let s2 = store.rollup_tier(atlas_tsdb::TIER_T1, now - t1_ret_ms, PIN_MARGIN_MS)?;
    let pins = store.pinned_windows(PIN_MARGIN_MS)?;
    let r0 = store.apply_block_retention_tier(atlas_tsdb::TIER_RAW, now - raw_ret_ms, &pins)?;
    let r1 = store.apply_block_retention_tier(atlas_tsdb::TIER_T1, now - t1_ret_ms, &pins)?;
    let r2 = store.apply_block_retention_tier(atlas_tsdb::TIER_T2, now - t2_ret_ms, &pins)?;
    if s1.consumed_blocks + s2.consumed_blocks + (r0 + r1 + r2) as u64 > 0 {
        tracing::info!(
            t0_to_t1_consumed = s1.consumed_blocks,
            t0_to_t1_produced = s1.produced_blocks,
            t1_to_t2_consumed = s2.consumed_blocks,
            t1_to_t2_produced = s2.produced_blocks,
            pinned_skipped = s1.pinned_skipped + s2.pinned_skipped,
            retention_deleted = (r0 + r1 + r2) as u64,
            "compaction pass"
        );
    }
    Ok(())
}

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
        gpu_permille: set.system.gpu_permille,
        gpu_dedicated_used: set.system.gpu_dedicated_used,
        gpu_shared_used: set.system.gpu_shared_used,
        gpu_memory_budget: set.system.gpu_dedicated_budget.saturating_add(set.system.gpu_shared_budget),
        gpu_throttling: if set.gpu.adapters.iter().any(|a| a.thermal_throttling == Some(true)) {
            Some(true)
        } else if set.gpu.adapters.iter().any(|a| a.thermal_throttling == Some(false)) {
            Some(false)
        } else { None },
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
        gpu_adapters: set.gpu.adapters.clone(),
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
        let _ = self.heads.append(SeriesKey::system(Metric::SysGpuPermille), ts_ms, sys.gpu_permille as f64);
        let _ = self.heads.append(SeriesKey::system(Metric::SysGpuDedicatedUsed), ts_ms, sys.gpu_dedicated_used as f64);
        let _ = self.heads.append(SeriesKey::system(Metric::SysGpuSharedUsed), ts_ms, sys.gpu_shared_used as f64);
        let _ = self.heads.append(SeriesKey::system(Metric::SysGpuMemoryUsed), ts_ms, sys.gpu_dedicated_used.saturating_add(sys.gpu_shared_used) as f64);
        let _ = self.heads.append(SeriesKey::system(Metric::SysGpuMemoryBudget), ts_ms, sys.gpu_memory_budget as f64);
        if let Some(v) = sys.gpu_throttling {
            let _ = self.heads.append(SeriesKey::system(Metric::SysGpuThrottling), ts_ms, if v { 1.0 } else { 0.0 });
        }
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
        let _ = self.heads.append(SeriesKey::new(Metric::GpuPermille, scope), ts_ms, m.gpu_permille as f64);
        let _ = self.heads.append(SeriesKey::new(Metric::GpuDedicatedBytes, scope), ts_ms, m.gpu_dedicated_bytes as f64);
        let _ = self.heads.append(SeriesKey::new(Metric::GpuSharedBytes, scope), ts_ms, m.gpu_shared_bytes as f64);
        self.scope_last_seen.insert(scope, ts_ms);
    }

    fn append_gpu_adapter(&mut self, ts_ms: i64, scope: i64, a: &atlas_collectors::GpuAdapterSample) {
        let mut add = |metric, value| { let _ = self.heads.append(SeriesKey::new(metric, scope), ts_ms, value); };
        add(Metric::GpuAdapterPermille, a.utilization_permille as f64);
        add(Metric::GpuAdapterDedicatedUsed, a.dedicated_used as f64);
        add(Metric::GpuAdapterSharedUsed, a.shared_used as f64);
        if let Some(v) = a.temperature_c { add(Metric::GpuAdapterTemperatureC, v); }
        if let Some(v) = a.power_w { add(Metric::GpuAdapterPowerW, v); }
        if let Some(v) = a.power_percent { add(Metric::GpuAdapterPowerPercent, v); }
        if let Some(v) = a.core_clock_mhz { add(Metric::GpuAdapterCoreClockMhz, v as f64); }
        if let Some(v) = a.memory_clock_mhz { add(Metric::GpuAdapterMemoryClockMhz, v as f64); }
        if let Some(v) = a.fan_rpm { add(Metric::GpuAdapterFanRpm, v as f64); }
        if let Some(v) = a.fan_percent { add(Metric::GpuAdapterFanPercent, v); }
        for temperature in &a.temperatures {
            match temperature.kind {
                atlas_collectors::GpuTemperatureKind::Memory => {
                    add(Metric::GpuAdapterMemoryTemperatureC, temperature.celsius);
                }
                atlas_collectors::GpuTemperatureKind::Hotspot => {
                    add(Metric::GpuAdapterHotspotTemperatureC, temperature.celsius);
                }
                _ => {}
            }
        }
        if let Some(v) = a.thermal_throttling { add(Metric::GpuAdapterThrottling, if v { 1.0 } else { 0.0 }); }
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

    // R3: one compaction pass at startup so a service that was down long enough
    // for data to age still demotes/sweeps promptly, then on an idle cadence
    // between flush windows (never on the sampling loop).
    if let Err(e) = run_compaction(
        &mut store,
        now_ms(),
        RAW_RETENTION_MS,
        T1_RETENTION_MS,
        T2_RETENTION_MS,
    ) {
        tracing::warn!("startup compaction pass failed: {e}");
    }
    let mut last_compaction = Instant::now();

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
            for adapter in &tick.gpu_adapters {
                let scope = store.upsert_gpu_adapter(
                    &adapter.stable_key(), &adapter.name, &adapter.driver_version,
                    adapter.active_display, adapter.physical_index, adapter.vendor_id,
                    adapter.device_id, adapter.pci_domain, adapter.pci_bus,
                    adapter.pci_device, adapter.pci_function, &adapter.driver_date,
                    tick.ts_ms,
                )?;
                bw.append_gpu_adapter(tick.ts_ms, scope, adapter);
            }
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

        // R3: idle-cadence compaction — roll aged tiers + sweep retention. Runs
        // at most every COMPACT_EVERY_MS, on the writer thread between flushes,
        // so it never competes with the 1 Hz sampler. Best-effort.
        if last_compaction.elapsed().as_millis() >= COMPACT_EVERY_MS {
            if let Err(e) = run_compaction(
                &mut store,
                now_ms(),
                RAW_RETENTION_MS,
                T1_RETENTION_MS,
                T2_RETENTION_MS,
            ) {
                tracing::warn!("compaction pass failed: {e}");
            }
            last_compaction = Instant::now();
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

    let now = now_ms();
    let cutoff = now - RETENTION_HOURS * 3_600_000;
    // Deprecated interim per-window tables (no longer written) still get swept.
    let pruned = store.apply_retention(cutoff)?;
    // R3: a final compaction pass rolls aged tiers + sweeps per-tier retention
    // (pin-aware) so a clean shutdown leaves the store in its demoted steady
    // state rather than a raw-only one.
    if let Err(e) = run_compaction(
        &mut store,
        now,
        RAW_RETENTION_MS,
        T1_RETENTION_MS,
        T2_RETENTION_MS,
    ) {
        tracing::warn!("shutdown compaction pass failed: {e}");
    }
    tracing::info!("shutdown compaction complete");
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
fn cmd_serve(pipe: Option<String>, db: PathBuf, duration: Option<u64>) -> Result<()> {
    let stop = install_ctrlc();
    serve_loop(pipe, db, stop, duration)
}

/// The serve core, driven by an externally owned `stop` flag so it can be hosted
/// both by the `serve` CLI command (Ctrl+C flag) and by the Windows service body
/// (SCM STOP/SHUTDOWN flag). Blocks until `stop` flips (or `duration` elapses),
/// then drains cleanly — a clean stop runs the rules engine's shutdown restore.
#[cfg(windows)]
fn serve_loop(
    pipe: Option<String>,
    db: PathBuf,
    stop: Arc<AtomicBool>,
    duration: Option<u64>,
) -> Result<()> {
    use atlas_ipc::{AtlasControlServer, AtlasPluginsServer, AtlasQueryServer, AtlasRulesServer};

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
        // The broker + rules service share the query service's store handle so the
        // audit log, history queries, and rule persistence use one connection.
        let broker = std::sync::Arc::new(broker::BrokerService::new(handle.store()));
        let rules = std::sync::Arc::new(rules_service::RulesService::new(
            handle.store(),
            handle.rules_engine(),
        ));
        // The signed plugin framework (R3, PRD §18.3): the AtlasPlugins
        // registry/session service + the in-memory session-token map it shares
        // with the capability interceptor. The interceptor wraps EVERY service so
        // a plugin token can only reach its granted AtlasQuery reads and is
        // rejected outright on the mutating / management surfaces.
        let sessions = std::sync::Arc::new(plugins::PluginSessions::new());
        let plugins_svc = std::sync::Arc::new(plugins::PluginsService::new(
            handle.store(),
            sessions.clone(),
        ));
        let store = handle.store();
        let router = tonic::transport::Server::builder()
            .add_service(plugins::PluginGuard::query(
                AtlasQueryServer::from_arc(handle.clone()),
                sessions.clone(),
                store.clone(),
            ))
            .add_service(plugins::PluginGuard::mutating(
                AtlasControlServer::from_arc(broker),
                sessions.clone(),
                store.clone(),
            ))
            .add_service(plugins::PluginGuard::mutating(
                AtlasRulesServer::from_arc(rules),
                sessions.clone(),
                store.clone(),
            ))
            .add_service(plugins::PluginGuard::mutating(
                AtlasPluginsServer::from_arc(plugins_svc),
                sessions.clone(),
                store.clone(),
            ));

        tracing::info!(pipe = %name, "AtlasQuery + AtlasControl + AtlasRules + AtlasPlugins serving");
        println!("Serving AtlasQuery + AtlasControl + AtlasRules + AtlasPlugins on {name}");

        // Optional self-stop after `duration` seconds (verification path): flip
        // the shared stop flag so the shutdown future below fires cleanly.
        if let Some(secs) = duration {
            let stop_timer = stop.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(secs)).await;
                tracing::info!(secs, "serve duration elapsed; stopping");
                stop_timer.store(true, Ordering::SeqCst);
            });
        }

        // Shut down when the shared stop flag flips (Ctrl+C in the CLI path, the
        // SCM STOP/SHUTDOWN control, or the duration timer). Poll at ~10 Hz.
        let shutdown = async move {
            while !stop.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        let result = atlas_ipc::serve(&name, router, shutdown).await;
        // Flip the sampler stop flag and block until the sampler thread has
        // restored every intervention before we return.
        handle.shutdown();
        handle.join_sampler();
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
fn cmd_serve(_pipe: Option<String>, _db: PathBuf, _duration: Option<u64>) -> Result<()> {
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

/// The Windows service name (SCM key) and display name. Must match the
/// `ServiceInstall`/`ServiceControl` Name in installer/Package.wxs so the
/// MSI-installed service and this CLI refer to the same service.
const SERVICE_NAME: &str = "SystemAtlas";
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

    // Serve on this thread until `stop` flips (the SCM owns the lifetime, so no
    // duration cap here).
    let serve_res = serve_loop(None, db, stop.clone(), None);

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

    // R3 tiered-retention footprint projection. This is a *retained footprint*
    // figure (30-day window, tiered vs raw-only), NOT a write-rate: the MB/day
    // WRITE budget above is unchanged by tiering — roll-up bounds how much
    // history is KEPT, not how fast it is written.
    let (raw_bps, t1_bpb, t2_bpb) = measure_tier_sizes();
    let (raw_only, tiered) = simulate_footprint(raw_bps, t1_bpb, t2_bpb, 30);
    let reduction = if raw_only > 0.0 {
        (raw_only - tiered) / raw_only * 100.0
    } else {
        0.0
    };
    println!(
        "retention tiers 30-day footprint/series: raw-only {:.2} MB -> tiered {:.2} MB ({:.1}% smaller)",
        raw_only / 1_048_576.0,
        tiered / 1_048_576.0,
        reduction
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

/// Per-series retained bytes under raw-only vs tiered retention over `sim_days`,
/// from measured encoded sizes: `raw_bps` = raw bytes/sample, `t1_bpb`/`t2_bpb`
/// = roll-up bytes per 10 s / 60 s bucket. Raw-only keeps 1 s samples for the
/// whole window; tiered keeps raw for [`RAW_RETENTION_MS`], T1 out to
/// [`T1_RETENTION_MS`], and T2 out to the window end. Pure + unit-tested.
fn simulate_footprint(raw_bps: f64, t1_bpb: f64, t2_bpb: f64, sim_days: u64) -> (f64, f64) {
    let total_s = (sim_days as i64 * 86_400) as f64;
    let raw_s = RAW_RETENTION_MS as f64 / 1000.0;
    let t1_s = T1_RETENTION_MS as f64 / 1000.0;

    // Raw-only: one 1 s sample per second for the whole window.
    let raw_only = raw_bps * total_s;

    // Tiered: raw for the recent window, then 10 s buckets, then 60 s buckets.
    let raw_window = total_s.min(raw_s);
    let t1_window = (total_s.min(t1_s) - raw_s).max(0.0);
    let t2_window = (total_s - t1_s).max(0.0);
    let tiered = raw_bps * raw_window + t1_bpb * (t1_window / 10.0) + t2_bpb * (t2_window / 60.0);
    (raw_only, tiered)
}

/// Encoded size of a representative synthetic 1-hour 1 s series at each tier, so
/// the footprint simulation uses real codec sizes rather than guesses. Returns
/// (raw_bytes_per_sample, t1_bytes_per_bucket, t2_bytes_per_bucket).
fn measure_tier_sizes() -> (f64, f64, f64) {
    use atlas_tsdb::{
        encode_rollup, rollup_buckets, rollup_raw, BlockBuilder, T1_BUCKET_SECS, T2_BUCKET_SECS,
    };
    let pts: Vec<(i64, f64)> = (0..3600)
        .map(|i| {
            (
                i * 1000,
                300.0 + (i as f64 * 0.13).sin() * 60.0 + (i % 7) as f64,
            )
        })
        .collect();
    let mut raw = BlockBuilder::new();
    for &(t, v) in &pts {
        let _ = raw.append(t, v);
    }
    let raw_bytes = raw.finish().len() as f64;
    let raw_bps = raw_bytes / pts.len() as f64;

    let t1 = rollup_raw(&pts, T1_BUCKET_SECS * 1000);
    let t1_bytes = encode_rollup(&t1, T1_BUCKET_SECS).len() as f64;
    let t1_bpb = t1_bytes / t1.len().max(1) as f64;

    let t2 = rollup_buckets(&t1, T2_BUCKET_SECS * 1000);
    let t2_bytes = encode_rollup(&t2, T2_BUCKET_SECS).len() as f64;
    let t2_bpb = t2_bytes / t2.len().max(1) as f64;
    (raw_bps, t1_bpb, t2_bpb)
}

/// `storage`: per-tier block/byte breakdown + a simulated tiered-vs-raw
/// footprint comparison (R3). Optionally forces a compaction pass first.
fn cmd_storage(
    db_path: PathBuf,
    rollup: bool,
    raw_retention_secs: u64,
    t1_retention_secs: u64,
    sim_days: u64,
) -> Result<()> {
    let mut store = Store::open(&db_path)?;

    if rollup {
        let before = store.block_counts_by_tier()?;
        let now = now_ms();
        run_compaction(
            &mut store,
            now,
            raw_retention_secs as i64 * 1000,
            t1_retention_secs as i64 * 1000,
            T2_RETENTION_MS,
        )?;
        let after = store.block_counts_by_tier()?;
        println!(
            "Forced compaction (raw_retention={raw_retention_secs}s, t1_retention={t1_retention_secs}s):"
        );
        println!(
            "  T0 blocks {} -> {}   T1 blocks {} -> {}   T2 blocks {} -> {}",
            before[0], after[0], before[1], after[1], before[2], after[2]
        );
    }

    let counts = store.block_counts_by_tier()?;
    let bytes = store.sample_storage_bytes_by_tier()?;
    let samples = store.sample_count()?;
    println!(
        "\nStored sample blocks by tier (source {}):",
        db_path.display()
    );
    println!("  {:<6} {:>10} {:>14}", "TIER", "BLOCKS", "BYTES");
    for (i, label) in ["T0 raw", "T1 10s", "T2 60s"].iter().enumerate() {
        println!("  {label:<6} {:>10} {:>14}", counts[i], bytes[i]);
    }
    println!(
        "  {:<6} {:>10} {:>14}   ({} samples on record)",
        "total",
        counts.iter().sum::<u64>(),
        bytes.iter().sum::<u64>(),
        samples
    );

    // Simulated footprint over a synthetic series, using real codec sizes.
    let (raw_bps, t1_bpb, t2_bpb) = measure_tier_sizes();
    let (raw_only, tiered) = simulate_footprint(raw_bps, t1_bpb, t2_bpb, sim_days);
    let reduction = if raw_only > 0.0 {
        (raw_only - tiered) / raw_only * 100.0
    } else {
        0.0
    };
    // Scale to a representative series count so the totals are concrete. Prefer
    // the live series count if present.
    let series = store.distinct_series_count().unwrap_or(0).max(1);
    println!(
        "\nSimulated {sim_days}-day footprint (per series; raw {raw_bps:.2} B/sample, \
         T1 {t1_bpb:.2} B/bucket, T2 {t2_bpb:.2} B/bucket):"
    );
    println!(
        "  raw-only (1 s kept {sim_days} d) : {:>10.2} MB/series   {:>10.2} MB × {series} series",
        raw_only / 1_048_576.0,
        raw_only / 1_048_576.0 * series as f64
    );
    println!(
        "  tiered (T0 72h / T1 14d / T2 90d): {:>10.2} MB/series   {:>10.2} MB × {series} series",
        tiered / 1_048_576.0,
        tiered / 1_048_576.0 * series as f64
    );
    println!(
        "  footprint reduction              : {reduction:>9.1}%   (tiering bounds the RETAINED \
         footprint over long windows; it does not change the MB/day WRITE rate)"
    );
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

/// Proto `CapabilityKind` label for CLI output.
fn capability_label(cap: i32) -> &'static str {
    match cap {
        0 => "all",
        1 => "camera",
        2 => "microphone",
        3 => "location",
        _ => "?",
    }
}

/// Proto `PrivacyAlertCondition` label for CLI output.
fn condition_label(cond: i32) -> &'static str {
    match cond {
        1 => "any-use",
        2 => "background",
        3 => "while-locked",
        4 => "unknown-app",
        5 => "longer-than",
        _ => "?",
    }
}

/// Parses a capability name into the proto `CapabilityKind` discriminant (0 = all).
fn parse_capability_filter(s: &str) -> Result<i32> {
    let v = match s.trim().to_ascii_lowercase().as_str() {
        "" | "all" => 0,
        "camera" | "cam" | "webcam" => 1,
        "microphone" | "mic" => 2,
        "location" | "loc" | "gps" => 3,
        other => anyhow::bail!("unknown capability '{other}' (camera|microphone|location|all)"),
    };
    Ok(v)
}

/// Parses a condition name into the proto `PrivacyAlertCondition` discriminant.
fn parse_alert_condition(s: &str) -> Result<i32> {
    let v = match s.trim().to_ascii_lowercase().as_str() {
        "any" | "any-use" | "anyuse" => 1,
        "background" | "bg" | "background-use" => 2,
        "locked" | "while-locked" | "whilelocked" => 3,
        "unknown" | "unknown-app" | "unsigned" => 4,
        "longer-than" | "longer" | "duration" => 5,
        other => anyhow::bail!(
            "unknown condition '{other}' (any-use|background|while-locked|unknown-app|longer-than)"
        ),
    };
    Ok(v)
}

/// `privacy-alert add|list|rm`: CRUD over the store's `privacy_alert_rule` table
/// (R2, PRD §9.10.3). Windows-only (the alert engine is a Windows feature).
#[cfg(windows)]
fn cmd_privacy_alert(db_path: PathBuf, cmd: PrivacyAlertCmd) -> Result<()> {
    use atlas_store::PrivacyAlertRuleRow;
    match cmd {
        PrivacyAlertCmd::Add {
            capability,
            condition,
            threshold,
            name,
            disabled,
        } => {
            let cap = parse_capability_filter(capability.as_deref().unwrap_or("all"))?;
            let cond = parse_alert_condition(&condition)?;
            let threshold_seconds = threshold.unwrap_or(0);
            if cond == 5 && threshold_seconds == 0 {
                anyhow::bail!(
                    "condition longer-than needs --threshold <seconds> (a positive value)"
                );
            }
            let rule_name = name
                .unwrap_or_else(|| format!("{} {}", capability_label(cap), condition_label(cond)));
            let store = Store::open(&db_path)?;
            let id = store.create_privacy_alert_rule(&PrivacyAlertRuleRow {
                id: 0,
                name: rule_name.clone(),
                enabled: !disabled,
                capability: cap,
                condition: cond,
                threshold_seconds,
                created_ms: 0,
            })?;
            println!(
                "added privacy-alert rule #{id} ({}): capability={} condition={} threshold={}s name='{}'",
                if disabled { "disabled" } else { "enabled" },
                capability_label(cap),
                condition_label(cond),
                threshold_seconds,
                rule_name,
            );
        }
        PrivacyAlertCmd::List => {
            let store = Store::open(&db_path)?;
            let rules = store.list_privacy_alert_rules()?;
            if rules.is_empty() {
                println!(
                    "No privacy-alert rules. Add one with `privacy-alert add --capability microphone --condition any-use`."
                );
            } else {
                for r in rules {
                    println!(
                        "#{:<4} {:<8} capability={:<10} condition={:<12} threshold={:<4}s name='{}'",
                        r.id,
                        if r.enabled { "ENABLED" } else { "disabled" },
                        capability_label(r.capability),
                        condition_label(r.condition),
                        r.threshold_seconds,
                        r.name,
                    );
                }
            }
        }
        PrivacyAlertCmd::Rm { id } => {
            let store = Store::open(&db_path)?;
            if store.delete_privacy_alert_rule(id)? {
                println!("deleted privacy-alert rule #{id}");
            } else {
                println!("no privacy-alert rule #{id}");
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_privacy_alert(_db_path: PathBuf, _cmd: PrivacyAlertCmd) -> Result<()> {
    anyhow::bail!("the `privacy-alert` command requires Windows");
}

/// `fired-alerts`: print recorded fired privacy alerts from the store (R2).
fn cmd_fired_alerts(db_path: PathBuf, minutes: u64, limit: u32) -> Result<()> {
    let store = Store::open(&db_path)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let from_ms = now - (minutes as i64) * 60_000;
    let (alerts, truncated) = store.list_fired_alerts(from_ms, now, limit)?;
    if alerts.is_empty() {
        println!("No fired privacy alerts in the last {minutes} minute(s).");
        return Ok(());
    }
    println!(
        "{} fired alert(s){}:",
        alerts.len(),
        if truncated { " (truncated)" } else { "" }
    );
    for a in &alerts {
        println!(
            "[{}] {:<10} rule='{}' app={} :: {}",
            format_ts(a.ts_ms),
            capability_label(a.capability),
            a.rule_name,
            truncate(&a.display_name, 30),
            a.detail,
        );
    }
    Ok(())
}

/// Short label for a proto `SystemChangeKind` discriminant (R3 dev output).
fn change_kind_name(kind: i32) -> &'static str {
    match kind {
        1 => "APP_INSTALLED",
        2 => "APP_UPDATED",
        3 => "APP_REMOVED",
        4 => "DRIVER_INSTALLED",
        5 => "DRIVER_UPDATED",
        6 => "WINDOWS_UPDATE",
        7 => "SERVICE_INSTALLED",
        8 => "SERVICE_CONFIG_CHANGED",
        9 => "SERVICE_REMOVED",
        10 => "STARTUP_ADDED",
        11 => "STARTUP_REMOVED",
        12 => "SCHEDULED_TASK_ADDED",
        13 => "SCHEDULED_TASK_REMOVED",
        14 => "POWER_PLAN_CHANGED",
        15 => "DEFAULT_APP_CHANGED",
        _ => "UNSPECIFIED",
    }
}

/// Short label for a proto `CrashKind` discriminant (R3 dev output).
fn crash_kind_name(kind: i32) -> &'static str {
    match kind {
        1 => "APP_CRASH",
        2 => "APP_HANG",
        3 => "BUGCHECK",
        4 => "SERVICE_FAILURE",
        5 => "UNEXPECTED_SHUTDOWN",
        _ => "UNSPECIFIED",
    }
}

/// `changes`: print recorded system changes from the store (R3, PRD §9.13).
fn cmd_changes(db_path: PathBuf, minutes: u64, limit: u32) -> Result<()> {
    let store = Store::open(&db_path)?;
    let now = now_ms();
    let from_ms = now - (minutes as i64) * 60_000;
    let (changes, truncated) = store.list_system_changes(from_ms, now, &[], limit)?;
    if changes.is_empty() {
        println!("No system changes recorded in the last {minutes} minute(s).");
        return Ok(());
    }
    println!(
        "{} system change(s){}:",
        changes.len(),
        if truncated { " (truncated)" } else { "" }
    );
    for c in &changes {
        let publisher = if c.publisher.is_empty() {
            String::new()
        } else {
            format!(" [{}]", c.publisher)
        };
        println!(
            "[{}] {:<22} {}{} :: {}",
            format_ts(c.ts_ms),
            change_kind_name(c.kind),
            truncate(&c.subject, 40),
            publisher,
            c.detail,
        );
    }
    Ok(())
}

/// `crashes`: print recorded crashes with their correlation context (R3,
/// PRD §9.14).
fn cmd_crashes(db_path: PathBuf, minutes: u64, limit: u32) -> Result<()> {
    let store = Store::open(&db_path)?;
    let now = now_ms();
    let from_ms = now - (minutes as i64) * 60_000;
    let (crashes, truncated) = store.list_crashes(from_ms, now, &[], limit)?;
    if crashes.is_empty() {
        println!("No crashes recorded in the last {minutes} minute(s).");
        println!(
            "(If `serve` has not run on this box, the crash scanner has not read the \
             reliability/WER logs yet.)"
        );
        return Ok(());
    }
    println!(
        "{} crash(es){}:",
        crashes.len(),
        if truncated { " (truncated)" } else { "" }
    );
    for c in &crashes {
        let fault = if c.fault.is_empty() {
            String::new()
        } else {
            format!(" fault={}", c.fault)
        };
        let exc = if c.exception_code.is_empty() {
            String::new()
        } else {
            format!(" {}", c.exception_code)
        };
        println!(
            "[{}] {:<20} {}{}{}",
            format_ts(c.ts_ms),
            crash_kind_name(c.kind),
            truncate(&c.subject, 40),
            fault,
            exc,
        );
        for line in &c.context {
            println!("      - {line}");
        }
    }
    Ok(())
}

/// `detect-changes`: run one change-detection pass directly (R3). Windows-only —
/// it collects the live OS inventory. Seeds the baseline on first run; shows the
/// diff on subsequent runs.
#[cfg(windows)]
fn cmd_detect_changes(db_path: PathBuf) -> Result<()> {
    use std::sync::{Arc, Mutex};
    let had_baseline = {
        let store = Store::open(&db_path)?;
        store.get_inventory("full")?.is_some()
    };
    let store = Arc::new(Mutex::new(Store::open(&db_path)?));
    let detector = forensics::ChangeDetector::new(store);
    let recorded = detector.detect_once(true);
    if !had_baseline {
        println!(
            "Seeded the inventory baseline (first pass — no diff). {recorded} \
             event-sourced change(s) imported. Run again to see changes."
        );
    } else {
        println!("Detection pass complete: {recorded} change(s) recorded.");
    }
    Ok(())
}

/// `detect-changes` requires the live OS inventory (Windows-only).
#[cfg(not(windows))]
fn cmd_detect_changes(_db_path: PathBuf) -> Result<()> {
    anyhow::bail!("the `detect-changes` command requires Windows");
}

/// `privacy-watch`: arm the ConsentStore change-watcher and print live
/// transitions until Ctrl+C (R2 verification path). Windows-only.
#[cfg(windows)]
fn cmd_privacy_watch() -> Result<()> {
    use atlas_collectors::PrivacyWatcher;
    let stop = install_ctrlc();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = PrivacyWatcher::spawn(stop.clone(), tx);
    println!("Watching ConsentStore camera/mic/location transitions (Ctrl+C to stop)...");
    println!("Trigger one by starting or stopping a mic/camera app.");
    while !stop.load(Ordering::SeqCst) {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(t) => {
                let cap = match t.capability {
                    atlas_collectors::Capability::Camera => "camera",
                    atlas_collectors::Capability::Microphone => "microphone",
                    atlas_collectors::Capability::Location => "location",
                };
                println!(
                    "[{}] {:<5} {:<10} {} (foreground={} locked={} active={}s)",
                    format_ts(t.ts_ms),
                    if t.started { "START" } else { "STOP" },
                    cap,
                    truncate(&t.display_name, 40),
                    t.foreground,
                    t.session_locked,
                    t.active_seconds,
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = handle.join();
    println!("privacy-watch stopped.");
    Ok(())
}

#[cfg(not(windows))]
fn cmd_privacy_watch() -> Result<()> {
    anyhow::bail!("the `privacy-watch` command requires Windows (ConsentStore registry)");
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

/// `inspect`: deep-inspect a process by pid (R2). Prints the detail plus any
/// requested sections. Windows-only (the inspector is Win32/NT FFI).
#[cfg(windows)]
fn cmd_inspect(
    pid: u32,
    handles: bool,
    modules: bool,
    threads: bool,
    handle_limit: u32,
) -> Result<()> {
    let res = atlas_collectors::process_detail(pid, 0);
    if !res.available {
        println!("Process {pid} unavailable: {}", res.unavailable_reason);
        return Ok(());
    }
    let d = res.detail.expect("available detail present");
    println!("== Process {} ({}) ==", d.pid, d.image_name);
    println!("  parent pid       : {}", d.parent_pid);
    println!("  image path       : {}", show(&d.image_path));
    println!("  command line     : {}", show(&d.command_line));
    println!("  working directory: {}", show(&d.working_directory));
    println!(
        "  user             : {} ({})",
        show(&d.user_name),
        show(&d.user_sid)
    );
    println!("  session          : {}", d.session_id);
    println!(
        "  integrity        : {}   elevated: {}",
        show(&d.integrity_level),
        d.elevated
    );
    println!("  architecture     : {}", show(&d.architecture));
    println!(
        "  signature        : {}   publisher: {}",
        show(&d.signature_status),
        show(&d.publisher)
    );
    println!(
        "  file version     : {}   product: {}",
        show(&d.file_version),
        show(&d.product_name)
    );
    println!(
        "  threads/handles  : {} / {}",
        d.thread_count, d.handle_count
    );
    println!("  start time (ms)  : {}", d.start_time_ms);
    println!("  package identity : {}", show(&d.package_identity));
    println!(
        "  coverage         : {}",
        if d.limited {
            "LIMITED (some fields need elevation / cross-user)"
        } else {
            "full"
        }
    );

    if threads {
        let ts = atlas_collectors::list_threads(pid);
        println!("\n-- Threads ({}) --", ts.len());
        println!(
            "{:>7} {:<14} {:<14} {:>4} {:>18} {:>10}",
            "TID", "STATE", "WAIT", "PRIO", "START ADDR", "CTXSW"
        );
        for t in ts.iter().take(200) {
            println!(
                "{:>7} {:<14} {:<14} {:>4} {:>#18x} {:>10}",
                t.tid, t.state, t.wait_reason, t.priority, t.start_address, t.context_switches
            );
        }
    }

    if modules {
        let res = atlas_collectors::list_modules(pid);
        if !res.available {
            println!("\n-- Modules unavailable: {} --", res.unavailable_reason);
        } else {
            println!("\n-- Modules ({}) --", res.modules.len());
            println!(
                "{:<32} {:<10} {:>18} {:>10} VERSION",
                "NAME", "SIGNED", "BASE", "SIZE KB"
            );
            for m in res.modules.iter().take(500) {
                println!(
                    "{:<32} {:<10} {:>#18x} {:>10} {}",
                    truncate(&m.name, 32),
                    if m.signed { "signed" } else { "unsigned" },
                    m.base_address,
                    m.size / 1024,
                    show(&m.version)
                );
            }
        }
    }

    if handles {
        let res = atlas_collectors::list_handles(pid, "", handle_limit);
        println!(
            "\n-- Handles ({}{}{}) --",
            res.handles.len(),
            if res.truncated { ", truncated" } else { "" },
            if res.names_limited {
                ", names_limited"
            } else {
                ""
            }
        );
        println!("{:>18} {:<16} {:>10} NAME", "HANDLE", "TYPE", "ACCESS");
        for h in res.handles.iter().take(500) {
            println!(
                "{:>#18x} {:<16} {:>#10x} {}",
                h.handle,
                truncate(&h.type_name, 16),
                h.granted_access,
                truncate(&h.name, 80)
            );
        }
    }

    Ok(())
}

#[cfg(not(windows))]
fn cmd_inspect(
    _pid: u32,
    _handles: bool,
    _modules: bool,
    _threads: bool,
    _handle_limit: u32,
) -> Result<()> {
    anyhow::bail!("the `inspect` command requires Windows (process inspector FFI)");
}

/// `security`: deep security detail for a process (R3, PRD §9.4.1/§9.4.6).
/// Windows-only. Prints the file hash, signature + certificate chain, token
/// privileges/groups/capabilities, and mitigations.
#[cfg(windows)]
fn cmd_security(pid: u32) -> Result<()> {
    let res = atlas_collectors::security_metadata(pid, 0);
    if !res.available {
        println!("Process {pid} unavailable: {}", res.unavailable_reason);
        return Ok(());
    }
    let m = res.metadata.expect("available metadata present");
    println!("== Security metadata for pid {pid} ==");
    println!("  file SHA-256     : {}", show(&m.file_sha256));
    println!(
        "  signature        : {}   (user {} / integrity {} / elevated {})",
        show(&m.signature_status),
        show(&m.user_sid),
        show(&m.integrity_level),
        m.elevated
    );
    println!(
        "  app container    : {}",
        if m.app_container { "yes" } else { "no" }
    );

    println!("\n-- Certificate chain ({}) --", m.cert_chain.len());
    if m.cert_chain.is_empty() {
        println!("  (no signing certificate chain — unsigned or unverifiable)");
    } else {
        for (i, c) in m.cert_chain.iter().enumerate() {
            let tag = if i == 0 {
                "leaf"
            } else if i + 1 == m.cert_chain.len() {
                "root"
            } else {
                "  "
            };
            println!(
                "  [{i}] {tag:<4} {} <= issued by {}",
                show(&c.subject),
                show(&c.issuer)
            );
            println!(
                "            sha1 {}   valid {}..{}",
                show(&c.thumbprint_sha1),
                c.not_before_ms,
                c.not_after_ms
            );
        }
    }

    println!("\n-- Token privileges ({}) --", m.privileges.len());
    for p in m.privileges.iter().take(200) {
        println!(
            "  {:<36} {}",
            p.name,
            if p.enabled { "enabled" } else { "disabled" }
        );
    }

    println!("\n-- Token groups ({}) --", m.groups.len());
    for g in m.groups.iter().take(200) {
        println!("  {g}");
    }

    println!("\n-- Capabilities ({}) --", m.capabilities.len());
    for c in m.capabilities.iter().take(200) {
        println!("  {c}");
    }

    println!("\n-- Mitigations ({}) --", m.mitigations.len());
    if m.mitigations.is_empty() {
        println!("  (none readable)");
    } else {
        println!("  {}", m.mitigations.join(", "));
    }

    println!(
        "\n  coverage         : {}",
        if m.limited {
            "LIMITED (some fields need elevation / cross-user)"
        } else {
            "full"
        }
    );
    Ok(())
}

#[cfg(not(windows))]
fn cmd_security(_pid: u32) -> Result<()> {
    anyhow::bail!("the `security` command requires Windows (security-metadata FFI)");
}

/// `locks`: find what is using a file/folder via the Restart Manager (R2).
/// Windows-only.
#[cfg(windows)]
fn cmd_locks(path: &str) -> Result<()> {
    let res = atlas_collectors::find_resource_owners(path);
    if !res.available {
        println!(
            "Resource-ownership search unavailable: {}",
            res.unavailable_reason
        );
        return Ok(());
    }
    if res.owners.is_empty() {
        println!("No process is currently using '{path}'.");
        return Ok(());
    }
    println!("{} owner(s) of '{path}':", res.owners.len());
    println!("{:>7} {:<8} {:<28} DESCRIPTION", "PID", "KIND", "IMAGE");
    for o in &res.owners {
        println!(
            "{:>7} {:<8} {:<28} {}",
            o.pid,
            if o.is_service { "service" } else { "app" },
            truncate(&o.image_name, 28),
            truncate(&o.description, 60)
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_locks(_path: &str) -> Result<()> {
    anyhow::bail!("the `locks` command requires Windows (Restart Manager)");
}

/// Human label for a collector L4 protocol.
#[cfg(windows)]
fn proto_label(p: atlas_collectors::NetL4Protocol) -> &'static str {
    match p {
        atlas_collectors::NetL4Protocol::Tcp => "TCP",
        atlas_collectors::NetL4Protocol::Udp => "UDP",
    }
}

/// Short human label for a collector TCP state.
#[cfg(windows)]
fn state_label(s: atlas_collectors::NetTcpState) -> &'static str {
    use atlas_collectors::NetTcpState as S;
    match s {
        S::Unspecified => "-",
        S::Closed => "CLOSED",
        S::Listen => "LISTEN",
        S::SynSent => "SYN_SENT",
        S::SynRcvd => "SYN_RCVD",
        S::Established => "ESTAB",
        S::FinWait1 => "FIN_WAIT1",
        S::FinWait2 => "FIN_WAIT2",
        S::CloseWait => "CLOSE_WAIT",
        S::Closing => "CLOSING",
        S::LastAck => "LAST_ACK",
        S::TimeWait => "TIME_WAIT",
        S::DeleteTcb => "DELETE_TCB",
    }
}

/// Formats an `addr:port` endpoint, bracketing IPv6.
#[cfg(windows)]
fn endpoint(addr: &str, port: u16, is_ipv6: bool) -> String {
    if addr.is_empty() {
        return "*".to_string();
    }
    if is_ipv6 {
        format!("[{addr}]:{port}")
    } else {
        format!("{addr}:{port}")
    }
}

/// `connections`: list TCP/UDP connections with owner + DNS-cache domains (R2).
#[cfg(windows)]
fn cmd_connections(listening: bool) -> Result<()> {
    let conns = atlas_collectors::list_connections(listening);
    if conns.is_empty() {
        println!("No connections found.");
        return Ok(());
    }
    let resolved = conns.iter().filter(|c| !c.remote_domain.is_empty()).count();
    println!(
        "{} connection(s){} — {} with a resolved domain (DNS cache):",
        conns.len(),
        if listening { " (incl. listening)" } else { "" },
        resolved
    );
    println!(
        "{:<5} {:>7} {:<22} {:<24} {:<11} {:<24} DOMAIN",
        "PROTO", "PID", "IMAGE", "LOCAL", "STATE", "REMOTE"
    );
    for c in &conns {
        println!(
            "{:<5} {:>7} {:<22} {:<24} {:<11} {:<24} {}",
            proto_label(c.protocol),
            c.pid,
            truncate(&c.image_name, 22),
            truncate(&endpoint(&c.local_addr, c.local_port, c.is_ipv6), 24),
            state_label(c.state),
            truncate(&endpoint(&c.remote_addr, c.remote_port, c.is_ipv6), 24),
            truncate(&c.remote_domain, 40),
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_connections(_listening: bool) -> Result<()> {
    anyhow::bail!("the `connections` command requires Windows (iphlpapi)");
}

/// `ports`: list listening TCP + bound UDP ports with owner (R2).
#[cfg(windows)]
fn cmd_ports() -> Result<()> {
    let ports = atlas_collectors::list_listening_ports();
    if ports.is_empty() {
        println!("No listening ports found.");
        return Ok(());
    }
    println!("{} listening endpoint(s):", ports.len());
    println!("{:<5} {:>7} {:<28} IMAGE", "PROTO", "PID", "BIND");
    for p in &ports {
        println!(
            "{:<5} {:>7} {:<28} {}",
            proto_label(p.protocol),
            p.pid,
            truncate(&endpoint(&p.bind_addr, p.port, p.is_ipv6), 28),
            truncate(&p.image_name, 40),
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_ports() -> Result<()> {
    anyhow::bail!("the `ports` command requires Windows (iphlpapi)");
}

/// `tasks`: list scheduled tasks via Task Scheduler COM (R2).
#[cfg(windows)]
fn cmd_tasks(filter: String) -> Result<()> {
    let tasks = atlas_collectors::enumerate_tasks(&filter);
    if tasks.is_empty() {
        if filter.is_empty() {
            println!("No scheduled tasks enumerated (is the Task Scheduler service running?).");
        } else {
            println!("No scheduled tasks match filter '{filter}'.");
        }
        return Ok(());
    }
    println!(
        "{} scheduled task(s){}:",
        tasks.len(),
        if filter.is_empty() {
            String::new()
        } else {
            format!(" matching '{filter}'")
        }
    );
    println!(
        "{:<3} {:<40} {:<13} {:<13} {:>7} {:<18} TRIGGERS",
        "EN", "PATH", "LAST RUN", "NEXT RUN", "RESULT", "AUTHOR"
    );
    for t in &tasks {
        println!(
            "{:<3} {:<40} {:<13} {:<13} {:>#7x} {:<18} {}",
            if t.enabled { "on" } else { "off" },
            truncate(&t.path, 40),
            if t.last_run_ms == 0 {
                "-".to_string()
            } else {
                format_ts(t.last_run_ms)
            },
            if t.next_run_ms == 0 {
                "-".to_string()
            } else {
                format_ts(t.next_run_ms)
            },
            t.last_result,
            truncate(&t.author, 18),
            truncate(&t.triggers, 40),
        );
    }
    // Show one task's detail (action + settings) so the full pull is visible.
    if let Some(t) = tasks
        .iter()
        .find(|t| !t.action.is_empty())
        .or(tasks.first())
    {
        println!("\nExample detail — {}", t.path);
        println!("  action        : {}", show(&t.action));
        println!(
            "  run highest   : {}   on idle: {}   wake to run: {}",
            t.run_as_highest, t.runs_on_idle, t.wakes_to_run
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_tasks(_filter: String) -> Result<()> {
    anyhow::bail!("the `tasks` command requires Windows (Task Scheduler COM)");
}

/// `boots`: report boot performance from the Diagnostics-Performance log (R2).
#[cfg(windows)]
fn cmd_boots(limit: u32) -> Result<()> {
    let a = atlas_collectors::analyze_boots(limit);
    if !a.available {
        println!("Boot analysis unavailable: {}", a.unavailable_reason);
        return Ok(());
    }
    if a.boots.is_empty() {
        println!("Boot analysis available, but no boot (event 100) records were found.");
        return Ok(());
    }
    println!("{} boot record(s), newest first:", a.boots.len());
    println!(
        "{:<20} {:>10} {:>12} {:>10} FLAG",
        "BOOT TIME (UTC t-of-day)", "TOTAL s", "MAIN PATH s", "POST s"
    );
    for b in &a.boots {
        println!(
            "{:<20} {:>10.1} {:>12.1} {:>10.1} {}",
            format_ts(b.boot_ms),
            b.boot_duration_ms as f64 / 1000.0,
            b.main_path_ms as f64 / 1000.0,
            b.post_boot_ms as f64 / 1000.0,
            if b.degraded { "SLOW" } else { "" }
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_boots(_limit: u32) -> Result<()> {
    anyhow::bail!("the `boots` command requires Windows (event log)");
}

/// `battery`: report battery status + health (R2).
#[cfg(windows)]
fn cmd_battery() -> Result<()> {
    let b = atlas_collectors::battery_status();
    if !b.available {
        println!("Battery unavailable: {}", b.unavailable_reason);
        return Ok(());
    }
    println!("Battery status:");
    println!(
        "  power        : {}   charging: {}",
        if b.on_ac { "AC" } else { "battery" },
        b.charging
    );
    println!("  charge       : {}%", b.percent);
    if b.rate_mw != 0 {
        println!("  rate         : {} mW", b.rate_mw);
    }
    if b.full_charge_mwh > 0 || b.design_mwh > 0 {
        println!(
            "  capacity     : {} / {} mWh (remaining / full charge)",
            b.remaining_mwh, b.full_charge_mwh
        );
        println!("  design       : {} mWh", b.design_mwh);
    }
    if b.health_percent > 0 {
        println!(
            "  health       : {}% (full charge ÷ design)",
            b.health_percent
        );
    }
    if b.cycle_count > 0 {
        println!("  cycle count  : {}", b.cycle_count);
    }
    if b.est_runtime_s > 0 {
        println!("  est. runtime : {} min", b.est_runtime_s / 60);
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_battery() -> Result<()> {
    anyhow::bail!("the `battery` command requires Windows (power APIs)");
}

/// `thermal`: report ACPI thermal-zone temperatures via WMI (R2).
#[cfg(windows)]
fn cmd_thermal() -> Result<()> {
    let t = atlas_collectors::thermal_status();
    if !t.available {
        println!("Thermal unavailable: {}", t.unavailable_reason);
        return Ok(());
    }
    println!("{} thermal sensor(s):", t.sensors.len());
    println!("{:<40} {:>10}  SOURCE", "SENSOR", "°C");
    for s in &t.sensors {
        println!(
            "{:<40} {:>10.1}  {}",
            truncate(&s.name, 40),
            s.celsius,
            s.source
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_thermal() -> Result<()> {
    anyhow::bail!("the `thermal` command requires Windows (WMI)");
}

/// Renders an empty string as a dim placeholder for the inspect output.
#[cfg(windows)]
fn show(s: &str) -> &str {
    if s.is_empty() {
        "-"
    } else {
        s
    }
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

// ---------------------------------------------------------------------------
// R2 rules-engine dev commands (docs/phases.md R2). CRUD + simulate operate on
// the store directly; `interventions` is a pipe client (the ledger lives inside
// the running `serve`); `policy` reads a process's current state via the FFI.
// ---------------------------------------------------------------------------

/// Parses a priority-class name into the proto `PriorityClass` discriminant.
#[cfg(windows)]
fn parse_priority(s: &str) -> Result<i32> {
    use atlas_ipc::PriorityClass as P;
    let v = match s.to_ascii_lowercase().as_str() {
        "idle" => P::PriorityIdle,
        "below-normal" | "below_normal" | "belownormal" | "below" => P::PriorityBelowNormal,
        "normal" => P::PriorityNormal,
        "above-normal" | "above_normal" | "abovenormal" | "above" => P::PriorityAboveNormal,
        "high" => P::PriorityHigh,
        other => {
            anyhow::bail!("unknown priority '{other}' (idle|below-normal|normal|above-normal|high)")
        }
    };
    Ok(v as i32)
}

/// Parses a trigger name into the proto `RuleTrigger` discriminant.
#[cfg(windows)]
fn parse_trigger(s: &str) -> Result<i32> {
    use atlas_ipc::RuleTrigger as T;
    let v = match s.to_ascii_lowercase().as_str() {
        "while-running" | "while" | "always" | "running" => T::WhileRunning,
        "ac" | "ac-power" | "on-ac" => T::OnAcPower,
        "dc" | "dc-power" | "on-dc" | "battery" => T::OnDcPower,
        "fullscreen" | "foreground" => T::OnFullscreen,
        "gpu-load" | "gpu" => T::OnGpuLoad,
        "gpu-thermal" | "gpu-throttle" => T::OnGpuThermalThrottle,
        other => {
            anyhow::bail!("unknown trigger '{other}' (while-running|ac|dc|fullscreen|gpu-load|gpu-thermal)")
        }
    };
    Ok(v as i32)
}

/// Parses an affinity-mode name into the proto `CoreAffinityMode` discriminant.
#[cfg(windows)]
fn parse_affinity(s: &str) -> Result<i32> {
    use atlas_ipc::CoreAffinityMode as A;
    let v = match s.to_ascii_lowercase().as_str() {
        "all" | "all-cores" => A::AllCores,
        "prefer-p" | "p" | "p-cores" => A::PreferPCores,
        "prefer-e" | "e" | "e-cores" => A::PreferECores,
        "custom" | "mask" => A::CustomMask,
        other => {
            anyhow::bail!("unknown affinity '{other}' (all|prefer-p|prefer-e|custom)")
        }
    };
    Ok(v as i32)
}

/// Parses a hex/decimal affinity bitmask (`0xF`, `15`).
#[cfg(windows)]
fn parse_mask(s: &str) -> Result<u64> {
    let t = s.trim();
    let v = if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)?
    } else {
        t.parse::<u64>()?
    };
    Ok(v)
}

/// Builds a store `RuleRow` from the shared authoring args.
#[cfg(windows)]
fn build_rule_row(args: &RuleArgs, enabled: bool) -> Result<atlas_store::RuleRow> {
    let match_image = args.match_image.clone().unwrap_or_default();
    if match_image.trim().is_empty() {
        anyhow::bail!("--match is required (an empty match would sweep nothing)");
    }
    let priority_class = match &args.priority {
        Some(s) => parse_priority(s)?,
        None => 0,
    };
    let (affinity_mode, affinity_mask) = match &args.affinity {
        Some(s) => {
            let mode = parse_affinity(s)?;
            let mask = if mode == atlas_ipc::CoreAffinityMode::CustomMask as i32 {
                let m = args
                    .mask
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("--affinity custom needs --mask"))?;
                parse_mask(m)?
            } else {
                0
            };
            (mode, mask)
        }
        None => (0, 0),
    };
    let trigger = match &args.trigger {
        Some(s) => parse_trigger(s)?,
        None => atlas_ipc::RuleTrigger::WhileRunning as i32,
    };
    Ok(atlas_store::RuleRow {
        id: 0,
        name: args.name.clone().unwrap_or_else(|| match_image.clone()),
        enabled,
        match_image,
        trigger,
        priority_class,
        affinity_mode,
        affinity_mask,
        eco_qos: args.eco,
        precedence: args.precedence,
        created_ms: 0,
        gpu_threshold_permille: 800,
        gpu_duration_seconds: 5,
    })
}

/// A one-line human description of a rule row for the CLI listing.
#[cfg(windows)]
fn describe_rule(r: &atlas_store::RuleRow) -> String {
    let trig = match rules::Trigger::from_disc(r.trigger) {
        rules::Trigger::WhileRunning => "while-running",
        rules::Trigger::OnAcPower => "on-AC",
        rules::Trigger::OnDcPower => "on-DC",
        rules::Trigger::OnFullscreen => "fullscreen",
        rules::Trigger::OnGpuLoad => "gpu-load",
        rules::Trigger::OnGpuThermalThrottle => "gpu-thermal-throttle",
    };
    let prio = rules::Priority::from_disc(r.priority_class).label();
    let aff = rules::Affinity::from_row(r.affinity_mode, r.affinity_mask).label();
    format!(
        "match={} trigger={} priority={} affinity={} eco={} precedence={}",
        r.match_image, trig, prio, aff, r.eco_qos, r.precedence
    )
}

#[cfg(windows)]
fn cmd_rule(db_path: PathBuf, cmd: RuleCmd) -> Result<()> {
    match cmd {
        RuleCmd::Add { args, disabled } => {
            let store = Store::open(&db_path)?;
            let row = build_rule_row(&args, !disabled)?;
            let id = store.create_rule(&row)?;
            println!(
                "added rule #{id} ({}): {}",
                if disabled { "disabled" } else { "enabled" },
                describe_rule(&row)
            );
        }
        RuleCmd::List => {
            let store = Store::open(&db_path)?;
            let rules = store.list_rules()?;
            if rules.is_empty() {
                println!("No rules. Add one with `rule add --match <image> --priority below-normal --eco`.");
            } else {
                for r in rules {
                    println!(
                        "#{:<4} {:<8} {}",
                        r.id,
                        if r.enabled { "ENABLED" } else { "disabled" },
                        describe_rule(&r)
                    );
                }
            }
        }
        RuleCmd::Enable { id } => {
            let store = Store::open(&db_path)?;
            let ok = store.set_rule_enabled(id, true)?;
            println!(
                "{}",
                if ok {
                    format!("rule #{id} enabled")
                } else {
                    format!("no rule #{id}")
                }
            );
        }
        RuleCmd::Disable { id } => {
            let store = Store::open(&db_path)?;
            let ok = store.set_rule_enabled(id, false)?;
            println!(
                "{}",
                if ok {
                    format!("rule #{id} disabled")
                } else {
                    format!("no rule #{id}")
                }
            );
        }
        RuleCmd::Rm { id } => {
            let store = Store::open(&db_path)?;
            let ok = store.delete_rule(id)?;
            println!(
                "{}",
                if ok {
                    format!("rule #{id} deleted")
                } else {
                    format!("no rule #{id}")
                }
            );
        }
        RuleCmd::Simulate { id, args } => cmd_rule_simulate(db_path, id, args)?,
    }
    Ok(())
}

#[cfg(windows)]
fn cmd_plugin(db_path: PathBuf, pipe: Option<String>, cmd: PluginCmd) -> Result<()> {
    use atlas_ipc::PluginCapability;

    match cmd {
        PluginCmd::Register {
            exe,
            caps,
            allow_unsigned,
        } => {
            let mask = plugins::parse_caps(&caps).map_err(|e| anyhow::anyhow!(e))?;
            let store = Store::open(&db_path)?;
            let out = plugins::register_plugin(&store, &exe, mask, allow_unsigned)?;
            println!("{}", out.message);
            if !out.ok {
                std::process::exit(1);
            }
        }
        PluginCmd::List => {
            let store = Store::open(&db_path)?;
            let rows = store.list_plugins()?;
            if rows.is_empty() {
                println!("No plugins. Register one with `plugin register <exe> --caps snapshot`.");
            } else {
                for r in rows {
                    let caps = plugins::mask_to_caps(r.granted_caps)
                        .iter()
                        .filter_map(|c| PluginCapability::try_from(*c).ok())
                        .map(plugins::cap_name)
                        .collect::<Vec<_>>()
                        .join(",");
                    let publisher = if r.publisher.is_empty() {
                        "<none>".to_string()
                    } else {
                        r.publisher.clone()
                    };
                    println!(
                        "#{:<4} {:<8} {:<9} {:<20} v{:<12} pub={:<28} caps=[{}]",
                        r.id,
                        if r.enabled { "ENABLED" } else { "disabled" },
                        plugins::sig_label(r.signature),
                        r.name,
                        r.version,
                        publisher,
                        caps,
                    );
                    println!("        exe: {}", r.exe_path);
                }
            }
        }
        PluginCmd::Enable { id } => {
            let store = Store::open(&db_path)?;
            let (_ok, message) = plugins::set_enabled(&store, id, true)?;
            println!("{message}");
        }
        PluginCmd::Disable { id } => {
            let store = Store::open(&db_path)?;
            let (_ok, message) = plugins::set_enabled(&store, id, false)?;
            println!("{message}");
        }
        PluginCmd::Grant { id, caps } => {
            let mask = plugins::parse_caps(&caps).map_err(|e| anyhow::anyhow!(e))?;
            let store = Store::open(&db_path)?;
            let (_ok, message) = plugins::grant_capabilities(&store, id, mask)?;
            println!("{message}");
        }
        PluginCmd::Rm { id } => {
            let store = Store::open(&db_path)?;
            let (_ok, message) = plugins::remove_plugin(&store, id)?;
            println!("{message}");
        }
        PluginCmd::Launch { id, print_nonce } => cmd_plugin_launch(db_path, pipe, id, print_nonce)?,
    }
    Ok(())
}

/// Mints a one-time launch nonce for plugin `id`, persists it (so the running
/// `serve` process can validate it in OpenPluginSession), and either prints it
/// or spawns the bundled example plugin with it. This is the launch handshake:
/// the nonce proves to `serve` that this launch was authorized. The plugin
/// exchanges it for a capability-scoped session token — the token itself is
/// never persisted, minted only inside the running service.
#[cfg(windows)]
fn cmd_plugin_launch(
    db_path: PathBuf,
    pipe: Option<String>,
    id: i64,
    print_nonce: bool,
) -> Result<()> {
    let store = Store::open(&db_path)?;
    let row = match store.get_plugin(id)? {
        Some(r) => r,
        None => {
            println!("no plugin #{id}");
            std::process::exit(1);
        }
    };
    if !row.enabled {
        println!("plugin #{id} '{}' is disabled — enable it first", row.name);
        std::process::exit(1);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let nonce = plugins::mint_launch_nonce(id);
    store.record_plugin_nonce(&nonce, id, now + plugins::LAUNCH_NONCE_TTL_MS)?;

    let pipe_name = resolve_pipe_name(pipe);

    if print_nonce {
        println!("Minted launch nonce for plugin #{id} '{}':", row.name);
        println!("  ATLAS_PLUGIN_ID={id}");
        println!("  ATLAS_PLUGIN_NONCE={nonce}");
        println!("  ATLAS_PLUGIN_PIPE={pipe_name}");
        println!(
            "Valid for {}s. The plugin calls OpenPluginSession({id}, nonce) to get its token.",
            plugins::LAUNCH_NONCE_TTL_MS / 1000
        );
        return Ok(());
    }

    // Spawn the bundled example plugin (built alongside this binary) with the
    // nonce in its environment.
    let exe = std::env::current_exe()?;
    let example = exe
        .parent()
        .map(|p| p.join("atlas-plugin-example.exe"))
        .ok_or_else(|| anyhow::anyhow!("cannot locate example plugin next to {}", exe.display()))?;
    if !example.is_file() {
        anyhow::bail!(
            "example plugin not found at {} — build it with `cargo build -p atlas-plugin-example`",
            example.display()
        );
    }
    println!(
        "Launching example plugin #{id} '{}' against {pipe_name} (nonce valid {}s)",
        row.name,
        plugins::LAUNCH_NONCE_TTL_MS / 1000
    );
    let status = std::process::Command::new(&example)
        .env("ATLAS_PLUGIN_ID", id.to_string())
        .env("ATLAS_PLUGIN_NONCE", &nonce)
        .env("ATLAS_PLUGIN_PIPE", &pipe_name)
        .status()?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

#[cfg(windows)]
fn cmd_rule_simulate(db_path: PathBuf, id: Option<i64>, args: Option<RuleArgs>) -> Result<()> {
    let store: ipc::SharedStore =
        std::sync::Arc::new(std::sync::Mutex::new(Store::open(&db_path)?));

    // Resolve the rule to simulate: a saved rule by id, or one built from flags.
    let sim_row = match (id, args) {
        (Some(id), _) => store
            .lock()
            .unwrap()
            .get_rule(id)?
            .ok_or_else(|| anyhow::anyhow!("no rule #{id}"))?,
        (None, Some(a)) => build_rule_row(&a, true)?,
        (None, None) => anyhow::bail!("provide --id <n> or the authoring flags (--match ...)"),
    };
    let sim = rules::ResolvableRule::from_row(&sim_row);
    let others: Vec<rules::ResolvableRule> = store
        .lock()
        .unwrap()
        .list_enabled_rules()?
        .iter()
        .filter(|r| sim_row.id == 0 || r.id != sim_row.id)
        .map(rules::ResolvableRule::from_row)
        .collect();

    let engine = rules::RulesEngine::new(store);
    let result = engine.simulate(&sim, &others);

    println!("=== Simulate rule: {} ===", describe_rule(&sim_row));
    if result.targets.is_empty() {
        println!("No live process currently matches this rule.");
    }
    for t in &result.targets {
        if t.blocked {
            println!(
                "  pid {:<6} {:<28} BLOCKED — {}",
                t.pid, t.image_name, t.blocked_reason
            );
        } else {
            println!(
                "  pid {:<6} {:<28} priority {} -> {} | affinity {} -> {} | eco change: {}",
                t.pid,
                t.image_name,
                t.current_priority,
                t.new_priority,
                t.current_affinity,
                t.new_affinity,
                t.eco_qos_change
            );
        }
    }
    if result.conflicts.is_empty() {
        println!("Conflicts: none");
    } else {
        println!("Conflicts:");
        for c in &result.conflicts {
            println!("  - {c}");
        }
    }
    Ok(())
}

#[cfg(windows)]
fn cmd_interventions(pipe: Option<String>) -> Result<()> {
    let name = resolve_pipe_name(pipe);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let channel = atlas_ipc::connect(&name)
            .await
            .map_err(|e| anyhow::anyhow!("connect to {name}: {e} (is `serve` running?)"))?;
        let mut client = atlas_ipc::AtlasRulesClient::new(channel);
        let reply = client
            .list_interventions(atlas_ipc::ListInterventionsRequest {})
            .await?
            .into_inner();
        if reply.interventions.is_empty() {
            println!("No live interventions.");
        } else {
            println!("Live interventions ({}):", reply.interventions.len());
            for i in reply.interventions {
                println!(
                    "  pid {:<6} {:<28} applied [{}] by rule #{} \"{}\"",
                    i.pid, i.image_name, i.applied, i.rule_id, i.rule_name
                );
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

#[cfg(windows)]
fn cmd_dynamic_protection(pipe: Option<String>, cmd: DynProtCmd) -> Result<()> {
    let name = resolve_pipe_name(pipe);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let channel = atlas_ipc::connect(&name)
            .await
            .map_err(|e| anyhow::anyhow!("connect to {name}: {e} (is `serve` running?)"))?;
        let mut client = atlas_ipc::AtlasRulesClient::new(channel);
        match cmd {
            DynProtCmd::Show => {
                let cfg = client
                    .get_dynamic_protection(atlas_ipc::GetDynamicProtectionRequest {})
                    .await?
                    .into_inner()
                    .config
                    .unwrap_or_default();
                println!("Dynamic responsiveness protection:");
                println!("  enabled                  : {}", cfg.enabled);
                println!(
                    "  cpu_threshold_permille   : {} ({}% of total CPU)",
                    cfg.cpu_threshold_permille,
                    cfg.cpu_threshold_permille / 10
                );
                println!("  sustain_seconds          : {}", cfg.sustain_seconds);
                println!(
                    "  max_intervention_seconds : {}",
                    cfg.max_intervention_seconds
                );
            }
            DynProtCmd::Set {
                enabled,
                threshold,
                sustain,
                max,
            } => {
                let reply = client
                    .set_dynamic_protection(atlas_ipc::SetDynamicProtectionRequest {
                        config: Some(atlas_ipc::DynamicProtectionConfig {
                            enabled,
                            cpu_threshold_permille: threshold,
                            sustain_seconds: sustain,
                            max_intervention_seconds: max,
                        }),
                    })
                    .await?
                    .into_inner();
                if reply.ok {
                    println!("OK: {}", reply.message);
                } else {
                    anyhow::bail!("rejected: {}", reply.message);
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    })?;
    Ok(())
}

#[cfg(windows)]
fn cmd_profile(db_path: PathBuf, cmd: ProfileCmd) -> Result<()> {
    match cmd {
        ProfileCmd::Add {
            name,
            rules,
            power_mode,
        } => {
            let mut store = Store::open(&db_path)?;
            let id = store.create_profile(&name, &power_mode, false, &rules)?;
            println!(
                "added profile #{id} '{name}' (power_mode='{power_mode}', {} rule(s))",
                rules.len()
            );
        }
        ProfileCmd::List => {
            let store = Store::open(&db_path)?;
            let profiles = store.list_profiles()?;
            if profiles.is_empty() {
                println!("No profiles.");
            } else {
                for p in profiles {
                    println!(
                        "#{:<4} {:<8} '{}' power_mode='{}' rules={:?}",
                        p.id,
                        if p.active { "ACTIVE" } else { "inactive" },
                        p.name,
                        p.power_mode,
                        p.rule_ids
                    );
                }
            }
        }
        ProfileCmd::Activate { id } => {
            let store = Store::open(&db_path)?;
            match rules_service::set_profile_active_impl(&store, id, true)? {
                Some(msg) => println!("{msg}"),
                None => println!("no profile #{id}"),
            }
        }
        ProfileCmd::Deactivate { id } => {
            let store = Store::open(&db_path)?;
            match rules_service::set_profile_active_impl(&store, id, false)? {
                Some(msg) => println!("{msg}"),
                None => println!("no profile #{id}"),
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn cmd_policy(pid: u32) -> Result<()> {
    use atlas_collectors::{
        eco_is_on, foreground_pid, get_affinity, get_eco_qos, get_priority_class, power_is_ac,
        priority_class_name,
    };
    let prio = get_priority_class(pid)
        .map(priority_class_name)
        .unwrap_or("<unreadable>");
    let aff = get_affinity(pid)
        .map(|a| {
            format!(
                "process=0x{:x} system=0x{:x}",
                a.process_mask, a.system_mask
            )
        })
        .unwrap_or_else(|| "<unreadable>".to_string());
    let eco = match get_eco_qos(pid) {
        Some(s) => {
            if eco_is_on(&s) {
                "on"
            } else {
                "off"
            }
        }
        None => "<unreadable>",
    };
    println!("Process {pid} policy:");
    println!("  priority : {prio}");
    println!("  affinity : {aff}");
    println!("  EcoQoS   : {eco}");
    println!(
        "Environment: power={} foreground_pid={}",
        match power_is_ac() {
            Some(true) => "AC",
            Some(false) => "battery",
            None => "unknown",
        },
        foreground_pid()
    );
    Ok(())
}

#[cfg(not(windows))]
fn cmd_rule(_db_path: PathBuf, _cmd: RuleCmd) -> Result<()> {
    anyhow::bail!("the `rule` command requires Windows");
}
#[cfg(not(windows))]
fn cmd_interventions(_pipe: Option<String>) -> Result<()> {
    anyhow::bail!("the `interventions` command requires Windows");
}
#[cfg(not(windows))]
fn cmd_profile(_db_path: PathBuf, _cmd: ProfileCmd) -> Result<()> {
    anyhow::bail!("the `profile` command requires Windows");
}
#[cfg(not(windows))]
fn cmd_dynamic_protection(_pipe: Option<String>, _cmd: DynProtCmd) -> Result<()> {
    anyhow::bail!("the `dynamic-protection` command requires Windows");
}
#[cfg(not(windows))]
fn cmd_policy(_pid: u32) -> Result<()> {
    anyhow::bail!("the `policy` command requires Windows");
}
#[cfg(not(windows))]
fn cmd_plugin(_db_path: PathBuf, _pipe: Option<String>, _cmd: PluginCmd) -> Result<()> {
    anyhow::bail!("the `plugin` command requires Windows");
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

/// Parses a support-bundle format token (html | json | text).
fn parse_bundle_format(token: &str) -> Result<atlas_ipc::ReportFormat> {
    Ok(match token.to_ascii_lowercase().as_str() {
        "html" => atlas_ipc::ReportFormat::ReportHtml,
        "json" => atlas_ipc::ReportFormat::ReportJson,
        "text" | "txt" => atlas_ipc::ReportFormat::ReportText,
        other => anyhow::bail!("unknown format '{other}'. Use: html | json | text"),
    })
}

/// Parses a comma-separated section list into SupportBundleSection discriminants.
/// An empty string yields an empty vec (which `selected` treats as "all").
fn parse_bundle_sections(csv: &str) -> Result<Vec<i32>> {
    use atlas_ipc::SupportBundleSection as S;
    let mut out = Vec::new();
    for tok in csv.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        let s = match tok.to_ascii_lowercase().as_str() {
            "device" | "device-info" => S::BundleDeviceInfo,
            "health" => S::BundleHealth,
            "incidents" => S::BundleIncidents,
            "changes" | "system-changes" => S::BundleSystemChanges,
            "crashes" => S::BundleCrashes,
            "services" => S::BundleServices,
            "startup" => S::BundleStartup,
            "self-metrics" | "self" => S::BundleSelfMetrics,
            other => anyhow::bail!(
                "unknown section '{other}'. Use: device, health, incidents, \
                 changes, crashes, services, startup, self-metrics"
            ),
        };
        out.push(s as i32);
    }
    Ok(out)
}

/// `support-bundle`: assemble a redacted diagnostic bundle straight from the
/// store + live OS reads (no `serve` needed) — the backend verification path.
#[cfg(windows)]
fn cmd_support_bundle(
    db_path: PathBuf,
    format: &str,
    minutes: u64,
    sections_csv: &str,
    out: Option<PathBuf>,
    redaction: atlas_ipc::RedactionOptions,
) -> Result<()> {
    use support_bundle::{
        BundleData, ConsumerRow, CrashesSection, DeviceSection, HealthSection, IncidentEntry,
        SelfMetricsSection,
    };

    let fmt = parse_bundle_format(format)?;
    let sel = support_bundle::selected(&parse_bundle_sections(sections_csv)?);
    let store = Store::open(&db_path)?;
    let now = now_ms();
    let from = now - (minutes as i64) * 60_000;
    let mem_total = current_mem_total();

    // Device.
    let device = if sel.device {
        let i = atlas_collectors::device_info();
        Some(DeviceSection {
            os_major: i.os_major,
            os_minor: i.os_minor,
            os_build: i.os_build,
            hostname: i.hostname,
            logical_cpus: i.logical_cpus,
            p_core_count: i.p_core_count,
            e_core_count: i.e_core_count,
            heterogeneous: i.heterogeneous,
            ram_total_bytes: i.ram_total_bytes,
            atlas_version: env!("CARGO_PKG_VERSION").to_string(),
            uptime_ms: i.uptime_ms,
        })
    } else {
        None
    };

    // Health: two samples ~400 ms apart so CPU rates are real, then top-10.
    let health = if sel.health {
        let mut sampler = Sampler::new()?;
        let _ = sampler.sample()?; // prime
        std::thread::sleep(std::time::Duration::from_millis(400));
        let set = sampler.sample()?;
        let mut procs: Vec<_> = set.processes.iter().collect();
        procs.sort_by(|a, b| {
            b.cpu_permille
                .cmp(&a.cpu_permille)
                .then(b.working_set.cmp(&a.working_set))
        });
        let top = procs
            .iter()
            .take(10)
            .map(|p| ConsumerRow {
                pid: p.key.pid,
                image_name: p.image_name.clone(),
                cpu_permille: p.cpu_permille,
                working_set: p.working_set,
                private_bytes: p.private_bytes,
            })
            .collect();
        let s = &set.system;
        let gpu_details = set.gpu.adapters.iter().map(|adapter| format!(
            "{} [{}] load={:.1}% temp={:?}C watts={:?} power_percent={:?} fan_rpm={:?} fan_percent={:?} core_clock={:?}MHz memory_clock={:?}MHz throttle={:?}; availability={:?}",
            adapter.name, adapter.stable_key(), adapter.utilization_permille as f64 / 10.0,
            adapter.temperature_c, adapter.power_w, adapter.power_percent, adapter.fan_rpm,
            adapter.fan_percent, adapter.core_clock_mhz, adapter.memory_clock_mhz,
            adapter.throttle_reasons, adapter.sensor_availability,
        )).collect();
        Some(HealthSection {
            ts_ms: set.ts_ms,
            cpu_permille: s.cpu_permille,
            mem_used: s.mem_used,
            mem_total: s.mem_total,
            commit_used: s.commit_used,
            commit_limit: s.commit_limit,
            process_count: s.process_count,
            thread_count: s.thread_count,
            handle_count: s.handle_count,
            gpu_permille: s.gpu_permille,
            gpu_dedicated_used: s.gpu_dedicated_used,
            gpu_dedicated_budget: s.gpu_dedicated_budget,
            gpu_shared_used: s.gpu_shared_used,
            gpu_shared_budget: s.gpu_shared_budget,
            gpu_details,
            top,
        })
    } else {
        None
    };

    // Incidents: refresh detection over the window (idempotent), list, diagnose.
    let incidents = if sel.incidents {
        let _ = detectors::run_detection_pass(&store, from, now, mem_total)?;
        let (rows, _truncated) = store.list_incidents(from, now, 200)?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let ctx = diagnostics::DiagnoseContext {
                kind: row.kind,
                start_ms: row.start_ms,
                end_ms: row.end_ms.unwrap_or(0),
                peak_value: row.peak_value,
            };
            let reply = diagnostics::diagnose(&store, &ctx, now, mem_total)?;
            entries.push(IncidentEntry {
                incident: incident_row_to_proto(&row),
                diagnosis: reply.diagnosis,
                unavailable_reason: reply.unavailable_reason,
            });
        }
        Some(entries)
    } else {
        None
    };

    let changes = if sel.changes {
        let (rows, _t) = store.list_system_changes(from, now, &[], 1000)?;
        Some(rows.iter().map(ipc::system_change_row_to_proto).collect())
    } else {
        None
    };

    let crashes = if sel.crashes {
        let (rows, _t) = store.list_crashes(from, now, &[], 1000)?;
        // The CLI path reads whatever the scanner has recorded; a serve-hosted
        // reply carries the scanner's availability, here we present what exists.
        Some(CrashesSection {
            available: true,
            unavailable_reason: String::new(),
            crashes: rows.iter().map(ipc::crash_row_to_proto).collect(),
        })
    } else {
        None
    };

    let services = if sel.services {
        Some(ipc::list_services_impl(""))
    } else {
        None
    };
    let startup = if sel.startup {
        Some(ipc::list_startup_impl())
    } else {
        None
    };

    let self_metrics = if sel.self_metrics {
        store.latest_self_sample()?.map(|s| SelfMetricsSection {
            ts_ms: s.ts_ms,
            cpu_permille: s.cpu_permille,
            working_set: s.working_set,
            tick_duration_us_avg: s.tick_duration_us_avg,
            tick_duration_us_max: s.tick_duration_us_max,
            ticks: s.ticks,
        })
    } else {
        None
    };

    let data = BundleData {
        range_from_ms: from,
        range_to_ms: now,
        device,
        health,
        incidents,
        changes,
        crashes,
        services,
        startup,
        self_metrics,
    };
    let reply = support_bundle::build_bundle(data, fmt, &redaction);

    match out {
        Some(path) => {
            std::fs::write(&path, reply.content.as_bytes())?;
            println!(
                "Wrote {} bundle ({}) to {} [redacted: {}]",
                format,
                reply.content_type,
                path.display(),
                if reply.redaction_applied.is_empty() {
                    "none".to_string()
                } else {
                    reply.redaction_applied.join(", ")
                }
            );
            println!("Suggested filename: {}", reply.filename);
        }
        None => print!("{}", reply.content),
    }
    Ok(())
}

/// The support bundle needs live OS reads (device info, inventories) — Windows-only.
#[cfg(not(windows))]
fn cmd_support_bundle(
    _db_path: PathBuf,
    _format: &str,
    _minutes: u64,
    _sections_csv: &str,
    _out: Option<PathBuf>,
    _redaction: atlas_ipc::RedactionOptions,
) -> Result<()> {
    anyhow::bail!("support-bundle is only supported on Windows")
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
            gpu_permille: 0,
            gpu_dedicated_used: 0,
            gpu_shared_used: 0,
            gpu_memory_budget: 0,
            gpu_throttling: None,
        }
    }

    fn proc_metrics(cpu: u32) -> ProcMetrics {
        ProcMetrics {
            cpu_permille: cpu,
            working_set: 100 << 20,
            private_bytes: 80 << 20,
            read_bps: 0,
            write_bps: 0,
            gpu_permille: 0,
            gpu_dedicated_bytes: 0,
            gpu_shared_bytes: 0,
        }
    }

    /// The tiered footprint simulation must show a large reduction over a 30-day
    /// window: raw kept 72 h, T1 (10 s) out to 14 d, T2 (60 s) beyond — the long
    /// tail collapses to a fraction of raw-only retention.
    #[test]
    fn simulated_footprint_shrinks_over_30_days() {
        let (raw_bps, t1_bpb, t2_bpb) = measure_tier_sizes();
        assert!(raw_bps > 0.0 && t1_bpb > 0.0 && t2_bpb > 0.0);
        let (raw_only, tiered) = simulate_footprint(raw_bps, t1_bpb, t2_bpb, 30);
        assert!(
            tiered < raw_only,
            "tiered must retain fewer bytes than raw-only"
        );
        // The 30-day tiered footprint should be well under half of raw-only —
        // most of the window is 60 s buckets, ~60× fewer points than 1 s raw.
        assert!(
            tiered * 2.0 < raw_only,
            "expected >50% footprint reduction: raw_only={raw_only} tiered={tiered}"
        );
    }

    /// A run shorter than the raw retention keeps everything raw — no coarser
    /// tier applies, so tiered == raw-only for that window.
    #[test]
    fn simulated_footprint_within_raw_window_equals_raw() {
        let (raw_bps, t1_bpb, t2_bpb) = measure_tier_sizes();
        // 1 day < 72 h raw retention → the whole window stays raw.
        let (raw_only, tiered) = simulate_footprint(raw_bps, t1_bpb, t2_bpb, 1);
        assert!((raw_only - tiered).abs() < 1e-6);
    }

    /// A head that reaches the point cap seals every system series
    /// series (one block each) and clears.
    #[test]
    fn block_writer_seals_sys_series_on_point_cap() {
        let mut bw = BlockWriter::new();
        for i in 0..SEAL_MAX_POINTS as i64 {
            bw.append_sys(1000 + i * 1000, &sys_metrics());
        }
        let blocks = bw.drain_sealed();
        // CPU/memory/count and GPU series, each sealed once at the cap.
        assert_eq!(blocks.len(), 11);
        assert!(blocks.iter().all(|b| b.points == SEAL_MAX_POINTS));
        assert!(blocks.iter().all(|b| b.key.scope == SYSTEM_SCOPE));
    }

    /// Draining a process scope on exit flushes its eight series and forgets it,
    /// so the cardinality guard no longer tracks it.
    #[test]
    fn block_writer_drains_scope_on_exit() {
        let mut bw = BlockWriter::new();
        bw.append_proc(1000, 42, &proc_metrics(500));
        bw.append_proc(2000, 42, &proc_metrics(400));
        let blocks = bw.drain_scope(42);
        assert_eq!(blocks.len(), 8, "eight per-process series including GPU");
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
        // Only scope 1's eight series are shed.
        assert_eq!(evicted.len(), 8);
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
