//! `atlas-mcp` — a read-only MCP server for Atlas (tech-stack.md §4.7).
//!
//! This is the R2 "bring your own AI client" adapter. A user registers this
//! binary in their MCP client (Claude Desktop, ChatGPT, any MCP host); the
//! client speaks MCP (JSON-RPC 2.0 over stdio) to us, and we translate each tool
//! call into **read-only** `AtlasQuery` RPCs over the existing named pipe. We
//! host NO model — the client's model does the reasoning and writes the answer.
//!
//! # Read-only by construction
//! This process only ever builds the `AtlasQuery` client. It never connects
//! `AtlasControl` or `AtlasRules`, so no tool call can suspend/kill a process or
//! change a rule. The tool catalogue is checked (at test time) to contain only
//! non-mutating query RPCs.
//!
//! # Privacy — the boundary got more important, not less
//! A tool result egresses to the client's model provider the moment the client
//! reads it. So redaction here is **default-ON and stricter than the in-app
//! views**: file paths, usernames, the computer name, DNS domains, command
//! lines, and (configurably) application names are scrubbed before anything
//! leaves this process. Relax individual axes with the `--no-redact-*` flags.
//!
//! # The honest limitation
//! Atlas guarantees its MCP tools return *grounded, citation-ready* evidence
//! (see each result's `grounding` block). It **cannot** guarantee the external
//! model's final answer contains no unsupported claims — Atlas controls the tool
//! results, the client controls the conversation and the response.

mod jsonrpc;
mod redact;
mod server;
mod tools;

use std::io::{self};

use clap::Parser;

use crate::redact::{RedactConfig, Redactor};
use crate::tools::Connection;

/// Read-only MCP server exposing grounded Atlas query tools to your own
/// MCP client. Speaks JSON-RPC 2.0 over stdio; connects to a running
/// `atlas-service serve` over its named pipe. Read-only by construction — no
/// tool can suspend, kill, or reconfigure anything.
///
/// HONEST LIMITATION: Atlas returns citation-ready evidence (see each result's
/// `grounding` block); it cannot guarantee your client model's final answer is
/// fully cited. Atlas controls the tool results, not the conversation.
#[derive(Parser, Debug)]
#[command(name = "atlas-mcp", version, about, long_about = None)]
struct Cli {
    /// Named-pipe discriminator of the running service (matches `serve --pipe`).
    /// Defaults to the current user's pipe, like the other Atlas clients.
    #[arg(long)]
    pipe: Option<String>,

    /// Keep file paths in tool output (default: redact to <PATH>).
    #[arg(long)]
    no_redact_paths: bool,

    /// Keep user names / SIDs in tool output (default: redact to <USER>).
    #[arg(long)]
    no_redact_user_names: bool,

    /// Keep the computer name in tool output (default: redact to <HOST>).
    #[arg(long)]
    no_redact_computer_name: bool,

    /// Keep DNS domains in tool output (default: redact to <DOMAIN>).
    #[arg(long)]
    no_redact_domains: bool,

    /// Keep command lines in tool output (default: redact to <CMD>).
    #[arg(long)]
    no_redact_command_lines: bool,

    /// Keep application / image names in tool output (default: redact to <APP>).
    #[arg(long)]
    no_redact_app_names: bool,
}

impl Cli {
    fn redact_config(&self) -> RedactConfig {
        RedactConfig {
            paths: !self.no_redact_paths,
            user_names: !self.no_redact_user_names,
            computer_name: !self.no_redact_computer_name,
            domains: !self.no_redact_domains,
            command_lines: !self.no_redact_command_lines,
            app_names: !self.no_redact_app_names,
        }
    }
}

/// Resolves the pipe name the same way the other Atlas clients do (username
/// scope by default), without depending on the service crate.
fn resolve_pipe_name(pipe: Option<String>) -> String {
    match pipe {
        Some(who) => atlas_ipc::pipe_name(&who),
        None => atlas_ipc::default_pipe_name(),
    }
}

fn main() -> anyhow::Result<()> {
    // Logs to stderr ONLY — stdout carries the JSON-RPC protocol stream.
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let cfg = cli.redact_config();
    let redactor = Redactor::new(cfg.clone());

    let pipe = resolve_pipe_name(cli.pipe.clone());
    tracing::info!(
        %pipe,
        redact_paths = cfg.paths,
        redact_user_names = cfg.user_names,
        redact_computer_name = cfg.computer_name,
        redact_domains = cfg.domains,
        redact_command_lines = cfg.command_lines,
        redact_app_names = cfg.app_names,
        "atlas-mcp starting (read-only AtlasQuery; MCP over stdio)"
    );

    let connection = Connection::new(pipe)?;

    let stdin = io::stdin();
    let stdout = io::stdout();
    server::run(stdin.lock(), stdout.lock(), connection, redactor)?;

    tracing::info!("atlas-mcp stdin closed; exiting");
    Ok(())
}
