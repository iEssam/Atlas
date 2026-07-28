//! `atlas` — the scriptable command-line interface for Atlas
//! (PRD §18.3 / §7.5, tech-stack §4.8).
//!
//! A thin, **read-only** clap CLI over the existing gRPC-over-named-pipe surface.
//! Each subcommand maps 1:1 onto a read-only `AtlasQuery` RPC (plus the
//! read-only `AtlasRules.ListRules`). Every command supports a global `--json`
//! flag that emits machine-readable JSON instead of the human table — this is
//! the scriptability / automation story (§18.3, §7.5; see also `atlas.psm1`).
//!
//! # Read-only by construction
//! The CLI never calls a mutating RPC: no `AtlasControl` (end task / suspend /
//! resume / terminate), no rule or profile create/update/delete/enable, no
//! privacy-alert mutation. Mutations are performed in the Atlas app. The
//! command→RPC mapping is a static table (`commands::COMMAND_RPCS`) and a unit
//! test asserts none of them carries a mutating verb.
//!
//! # Connecting
//! `--pipe <disc>` selects the service's named pipe (matching `serve --pipe`);
//! the default is the current user's pipe, like every other Atlas client. When
//! the service isn't running the CLI prints a clear message and exits non-zero.

mod client;
mod commands;
mod render;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use client::Connection;

/// Atlas CLI — scriptable, read-only queries over the running service.
///
/// Read-only: this CLI only ever runs query RPCs. Mutations (end task, applying
/// rules, privacy-alert changes) are performed in the Atlas app, never here.
/// Add `--json` to any command for machine-readable output.
#[derive(Parser, Debug)]
#[command(name = "atlas", version, about, long_about = None)]
#[command(
    after_help = "READ-ONLY: this CLI never changes anything. End-task, rules, and \
privacy-alert changes are done in the Atlas app. Add --json to any command for \
scriptable output."
)]
struct Cli {
    /// Named-pipe discriminator of the running service (matches `serve --pipe`).
    /// Defaults to the current user's pipe, like the other Atlas clients.
    #[arg(long, global = true)]
    pipe: Option<String>,

    /// Emit machine-readable JSON instead of the human table (for scripting).
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Top processes by CPU right now, with a system gauge summary (GetSnapshot).
    Top {
        /// Max processes to return (0 = all).
        #[arg(long, default_value_t = 15)]
        limit: u32,
    },
    /// Listening TCP/UDP ports with owning process (ListListeningPorts).
    Ports,
    /// Active TCP/UDP connections with owning process (ListConnections).
    Connections,
    /// Find which processes are holding a file open (FindResourceOwners).
    Locks {
        /// Path to the file/directory to inspect.
        path: String,
    },
    /// Historical min/max/avg for a metric over the last N minutes (QueryRange).
    History {
        /// Metric id or alias: sys-cpu, sys-mem, sys-commit, sys-process-count,
        /// cpu, working-set, private-bytes, read-bps, write-bps.
        #[arg(long)]
        metric: String,
        /// Look-back window in minutes.
        #[arg(long, default_value_t = 60)]
        minutes: i64,
        /// Process instance row id for process-scoped metrics (0 = system).
        #[arg(long, default_value_t = 0)]
        scope: i64,
        /// Decimation target (0 = server default).
        #[arg(long, default_value_t = 0)]
        buckets: u32,
    },
    /// Detected incidents in the last N minutes (ListIncidents).
    Incidents {
        /// Look-back window in minutes.
        #[arg(long, default_value_t = 1440)]
        minutes: i64,
        /// Max incidents to return.
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Evidence-based diagnosis of an incident by id (Diagnose).
    Diagnose {
        /// Incident id to diagnose (see `atlas incidents`).
        #[arg(long)]
        incident: i64,
    },
    /// Windows services inventory (ListServices).
    Services {
        /// Case-insensitive substring over name/display name.
        #[arg(long, default_value = "")]
        filter: String,
    },
    /// Startup inventory: Run keys, Startup folders, StartupApproved (ListStartup).
    Startup,
    /// Scheduled tasks (ListScheduledTasks).
    Tasks {
        /// Case-insensitive substring over name/path.
        #[arg(long, default_value = "")]
        filter: String,
    },
    /// Full-text search over processes, events, and bookmarks (Search).
    Search {
        /// Query string.
        query: String,
        /// Max hits to return.
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// List performance rules — READ ONLY (AtlasRules.ListRules). Create/enable
    /// rules in the Atlas app.
    Rules,
    /// Service version + advertised capability flags (GetCapabilities).
    Capabilities,
}

fn resolve_pipe_name(pipe: Option<String>) -> String {
    match pipe {
        Some(who) => atlas_ipc::pipe_name(&who),
        None => atlas_ipc::default_pipe_name(),
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let pipe = resolve_pipe_name(cli.pipe);
    let mut conn = Connection::new(pipe)?;
    let j = cli.json;
    match cli.command {
        Command::Top { limit } => commands::top(&mut conn, j, limit),
        Command::Ports => commands::ports(&mut conn, j),
        Command::Connections => commands::connections(&mut conn, j),
        Command::Locks { path } => commands::locks(&mut conn, j, path),
        Command::History {
            metric,
            minutes,
            scope,
            buckets,
        } => commands::history(&mut conn, j, metric, minutes, scope, buckets),
        Command::Incidents { minutes, limit } => commands::incidents(&mut conn, j, minutes, limit),
        Command::Diagnose { incident } => commands::diagnose(&mut conn, j, incident),
        Command::Services { filter } => commands::services(&mut conn, j, filter),
        Command::Startup => commands::startup(&mut conn, j),
        Command::Tasks { filter } => commands::tasks(&mut conn, j, filter),
        Command::Search { query, limit } => commands::search(&mut conn, j, query, limit),
        Command::Rules => commands::rules(&mut conn, j),
        Command::Capabilities => commands::capabilities(&mut conn, j),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("atlas: {e:#}");
            ExitCode::FAILURE
        }
    }
}
