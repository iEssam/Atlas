//! The read-only connection to the Atlas service.
//!
//! [`Connection`] owns a tokio runtime and lazily dials the service's named
//! pipe on first use. It exposes two typed clients:
//!
//! * `AtlasQueryClient` — the entire read surface (snapshots, history, events,
//!   incidents, diagnosis, inventories, network, search, capabilities).
//! * `AtlasRulesClient` — built **only** so the CLI can call the read-only
//!   `ListRules` RPC. The CLI never calls a mutating method on it (no
//!   create/update/delete/enable, no profile mutation). The read-only guarantee
//!   is enforced structurally by the command→RPC table in `commands.rs` and its
//!   test.
//!
//! A missing service surfaces as a clean, actionable error — never a panic.

use anyhow::{anyhow, Context, Result};
use tonic::transport::Channel;

use atlas_ipc::{AtlasQueryClient, AtlasRulesClient};

/// Lazily-connected, read-only client wrapper over the Atlas service pipe.
pub struct Connection {
    rt: tokio::runtime::Runtime,
    pipe: String,
    channel: Option<Channel>,
}

impl Connection {
    /// Creates the runtime; does not connect yet (connection is deferred to the
    /// first RPC so `--help` and arg errors never touch the pipe).
    pub fn new(pipe: String) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("build tokio runtime")?;
        Ok(Self {
            rt,
            pipe,
            channel: None,
        })
    }

    /// Dials the named pipe on first call and caches the shared channel.
    fn ensure_channel(&mut self) -> Result<Channel> {
        if self.channel.is_none() {
            let channel = self
                .rt
                .block_on(atlas_ipc::connect(&self.pipe))
                .map_err(|e| {
                    anyhow!(
                        "cannot reach the Atlas service on pipe '{}': {e}. Is `atlas-service serve` running?",
                        self.pipe
                    )
                })?;
            self.channel = Some(channel);
        }
        Ok(self.channel.as_ref().unwrap().clone())
    }

    /// Runs one `AtlasQuery` RPC, blocking on the runtime. The read surface.
    pub fn query<T, F, Fut>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(AtlasQueryClient<Channel>) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
    {
        let channel = self.ensure_channel()?;
        let client = AtlasQueryClient::new(channel);
        let resp = self
            .rt
            .block_on(f(client))
            .map_err(|status| anyhow!("{}: {}", status.code(), status.message()))?;
        Ok(resp.into_inner())
    }

    /// Runs one `AtlasRules` RPC, blocking on the runtime. The CLI only ever
    /// passes the read-only `ListRules` call here (see `commands::rules`).
    pub fn rules<T, F, Fut>(&mut self, f: F) -> Result<T>
    where
        F: FnOnce(AtlasRulesClient<Channel>) -> Fut,
        Fut: std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
    {
        let channel = self.ensure_channel()?;
        let client = AtlasRulesClient::new(channel);
        let resp = self
            .rt
            .block_on(f(client))
            .map_err(|status| anyhow!("{}: {}", status.code(), status.message()))?;
        Ok(resp.into_inner())
    }
}
