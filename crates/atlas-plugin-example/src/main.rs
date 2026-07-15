//! `atlas-plugin-example` — the living proof of the signed plugin framework
//! (docs/phases.md Phase 3 / R3, PRD §18.3, tech-stack §4.6).
//!
//! A tiny out-of-process READ-ONLY plugin. Launched with a one-time nonce (via
//! `atlas-service plugin launch <id>`, which passes `ATLAS_PLUGIN_ID`,
//! `ATLAS_PLUGIN_NONCE`, `ATLAS_PLUGIN_PIPE` in the environment), it:
//!
//!   1. exchanges the nonce for a capability-scoped session token
//!      (`OpenPluginSession`);
//!   2. calls a GRANTED AtlasQuery read (`GetSnapshot`) WITH the token in
//!      metadata — this SUCCEEDS;
//!   3. attempts an UNGRANTED AtlasQuery read (`Search`) — REJECTED by the
//!      server interceptor with PermissionDenied;
//!   4. attempts a MUTATION on the query surface (`CreateBookmark`) — REJECTED;
//!   5. attempts to reach a mutating service (`AtlasRules::ListRules`) — REJECTED
//!      outright (plugins are read-only, full stop).
//!
//! Every probe's outcome is compared against what the returned grant set says it
//! SHOULD be; the process exits non-zero if any probe deviates. This is the
//! end-to-end enforcement proof — the rejection is the deliverable.

#[cfg(windows)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    use atlas_ipc::{
        AtlasPluginsClient, AtlasQueryClient, AtlasRulesClient, CreateBookmarkRequest,
        ListRulesRequest, OpenPluginSessionRequest, PluginCapability, SearchRequest,
        SnapshotRequest, PLUGIN_TOKEN_METADATA_KEY,
    };
    use tonic::metadata::MetadataValue;
    use tonic::Request;

    let plugin_id: i64 = std::env::var("ATLAS_PLUGIN_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let nonce = std::env::var("ATLAS_PLUGIN_NONCE").unwrap_or_default();
    let pipe = std::env::var("ATLAS_PLUGIN_PIPE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(atlas_ipc::default_pipe_name);

    if plugin_id == 0 || nonce.is_empty() {
        anyhow::bail!(
            "missing ATLAS_PLUGIN_ID / ATLAS_PLUGIN_NONCE — launch me with \
             `atlas-service plugin launch <id>`"
        );
    }

    println!("[plugin] connecting to {pipe} as plugin #{plugin_id}");
    let channel = atlas_ipc::connect(&pipe)
        .await
        .map_err(|e| anyhow::anyhow!("connect {pipe}: {e}"))?;

    // Step 1: exchange the launch nonce for a capability-scoped session token.
    let mut plugins = AtlasPluginsClient::new(channel.clone());
    let session = plugins
        .open_plugin_session(OpenPluginSessionRequest {
            plugin_id,
            launch_nonce: nonce,
        })
        .await
        .map_err(|e| anyhow::anyhow!("OpenPluginSession: {e}"))?
        .into_inner();
    if !session.ok {
        anyhow::bail!("session refused: {}", session.message);
    }
    let token = session.session_token;
    let granted: Vec<PluginCapability> = session
        .granted
        .iter()
        .filter_map(|c| PluginCapability::try_from(*c).ok())
        .collect();
    let granted_names: Vec<&str> = granted.iter().map(|c| cap_name(*c)).collect();
    println!(
        "[plugin] session opened; granted caps = [{}]",
        granted_names.join(",")
    );
    let has = |c: PluginCapability| granted.contains(&c);

    // Helper: build a request carrying the plugin token in gRPC metadata. A
    // generic `fn` (not a closure) so it works for every request message type.
    let tok = MetadataValue::try_from(token.as_str())?;
    fn with_token<T>(msg: T, tok: &MetadataValue<tonic::metadata::Ascii>) -> Request<T> {
        let mut req = Request::new(msg);
        req.metadata_mut()
            .insert(PLUGIN_TOKEN_METADATA_KEY, tok.clone());
        req
    }

    let mut query = AtlasQueryClient::new(channel.clone());
    let mut rules = AtlasRulesClient::new(channel);

    let mut probes: Vec<Probe> = Vec::new();

    // Step 2: a GRANTED read — GetSnapshot (needs PLUGIN_CAP_SNAPSHOT).
    {
        let expect_allow = has(PluginCapability::PluginCapSnapshot);
        let outcome = query
            .get_snapshot(with_token(SnapshotRequest { top_n: 5 }, &tok))
            .await
            .map(|r| format!("{} rows", r.into_inner().processes.len()));
        probes.push(Probe::new(
            "GetSnapshot (granted read)",
            expect_allow,
            outcome,
        ));
    }

    // Step 3: an UNGRANTED read — Search (needs PLUGIN_CAP_SEARCH).
    {
        let expect_allow = has(PluginCapability::PluginCapSearch);
        let outcome = query
            .search(with_token(
                SearchRequest {
                    query: "chrome".to_string(),
                    limit: 5,
                },
                &tok,
            ))
            .await
            .map(|r| format!("{} hits", r.into_inner().hits.len()));
        probes.push(Probe::new("Search (ungranted read)", expect_allow, outcome));
    }

    // Step 4: a MUTATION on the query surface — CreateBookmark. Never allowed to
    // a plugin (no capability maps to a write).
    {
        let outcome = query
            .create_bookmark(with_token(
                CreateBookmarkRequest {
                    ts_ms: 0,
                    label: "plugin-should-not-write".to_string(),
                },
                &tok,
            ))
            .await
            .map(|r| format!("created #{}", r.into_inner().id));
        probes.push(Probe::new("CreateBookmark (mutation)", false, outcome));
    }

    // Step 5: reach a mutating SERVICE — AtlasRules::ListRules. Rejected outright
    // for any plugin token (plugins are read-only, full stop).
    {
        let outcome = rules
            .list_rules(with_token(ListRulesRequest {}, &tok))
            .await
            .map(|r| format!("{} rules", r.into_inner().rules.len()));
        probes.push(Probe::new(
            "AtlasRules.ListRules (mutating service)",
            false,
            outcome,
        ));
    }

    println!("\n[plugin] enforcement results:");
    let mut all_ok = true;
    for p in &probes {
        let (verdict, line) = p.report();
        if !verdict {
            all_ok = false;
        }
        println!("  {line}");
    }

    if all_ok {
        println!("\n[plugin] PASS — the capability scope was enforced exactly as granted.");
        Ok(())
    } else {
        eprintln!("\n[plugin] FAIL — an outcome deviated from the granted scope.");
        std::process::exit(1);
    }
}

#[cfg(windows)]
fn cap_name(c: atlas_ipc::PluginCapability) -> &'static str {
    use atlas_ipc::PluginCapability::*;
    match c {
        Unspecified => "unspecified",
        PluginCapSnapshot => "snapshot",
        PluginCapHistory => "history",
        PluginCapSearch => "search",
        PluginCapIncidents => "incidents",
        PluginCapInventory => "inventory",
        PluginCapNetwork => "network",
        PluginCapForensics => "forensics",
    }
}

/// One probe: what we expected (allow vs. deny) and what actually happened.
#[cfg(windows)]
struct Probe {
    label: String,
    expect_allow: bool,
    allowed: bool,
    detail: String,
}

#[cfg(windows)]
impl Probe {
    fn new(label: &str, expect_allow: bool, outcome: Result<String, tonic::Status>) -> Self {
        let (allowed, detail) = match outcome {
            Ok(msg) => (true, format!("ALLOWED ({msg})")),
            Err(status) => (
                false,
                format!("DENIED [{:?}] {}", status.code(), status.message()),
            ),
        };
        Self {
            label: label.to_string(),
            expect_allow,
            allowed,
            detail,
        }
    }

    /// Returns (matched_expectation, human line).
    fn report(&self) -> (bool, String) {
        let matched = self.allowed == self.expect_allow;
        let want = if self.expect_allow {
            "expected ALLOW"
        } else {
            "expected DENY"
        };
        let mark = if matched { "OK  " } else { "BAD " };
        (
            matched,
            format!("[{mark}] {:<40} {want:<15} -> {}", self.label, self.detail),
        )
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("atlas-plugin-example requires Windows (named-pipe transport).");
    std::process::exit(1);
}
