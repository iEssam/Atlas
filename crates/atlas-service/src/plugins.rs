//! The signed plugin framework (docs/phases.md Phase 3 / R3, PRD §18.3,
//! tech-stack §4.6). Out-of-process, Authenticode-signed, capability-scoped
//! READ-ONLY extensions.
//!
//! This module is the security-critical core of the framework. It has three
//! parts:
//!
//! 1. **The registry + signature gate** ([`register_plugin`], [`set_enabled`],
//!    [`grant_capabilities`], [`remove_plugin`]). A plugin executable's
//!    Authenticode signature is verified (via [`atlas_collectors::verify_signature_info`],
//!    the same primitive the process-detail collector uses) *before* it can be
//!    registered; an unsigned executable is REFUSED unless the user explicitly
//!    opts in. The signing publisher is taken from the verified certificate, and
//!    enabling re-verifies the signature (refusing if it degraded from the
//!    signed state it had at registration). Every register / grant / enable /
//!    remove / session-open / rejected-call is written to the audit log.
//!
//! 2. **Capability-scoped session tokens** ([`PluginSessions`]). An enabled
//!    plugin is launched with a one-time launch nonce (persisted in the store so
//!    the separate launcher process and this service agree on it — see
//!    [`crate::plugins`] docs on the launch handshake). It exchanges the nonce
//!    for an in-memory session token bound to (plugin_id, granted caps) with a
//!    short TTL via `OpenPluginSession`. The token — the actual capability grant
//!    — is NEVER persisted.
//!
//! 3. **The interceptor** ([`PluginGuard`]). A tonic/tower service wrapper on the
//!    router. A request carrying the `atlas-plugin-token` metadata may only reach
//!    the granted read-only slice of AtlasQuery (each RPC maps to a
//!    [`PluginCapability`] group per the proto security-model comment); anything
//!    else is rejected with `PermissionDenied` and audited. A plugin token
//!    presented to AtlasControl / AtlasRules / AtlasPlugins is rejected outright —
//!    plugins are read-only, full stop. A request with no plugin token is
//!    first-party (UI / CLI) and keeps full read access.

#![cfg(windows)]

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tonic::body::Body;
use tonic::codegen::http;
use tonic::codegen::Service;
use tonic::codegen::{Context, Poll};
use tonic::server::NamedService;
use tonic::{Request, Response, Status};

use atlas_collectors::{read_version_info, verify_signature_info, SignatureStatus};
use atlas_ipc::v0::atlas_plugins_server::AtlasPlugins;
use atlas_ipc::{
    GrantPluginCapabilitiesReply, GrantPluginCapabilitiesRequest, ListPluginsReply,
    ListPluginsRequest, OpenPluginSessionReply, OpenPluginSessionRequest, Plugin, PluginCapability,
    PluginSignature, RegisterPluginReply, RegisterPluginRequest, RemovePluginReply,
    RemovePluginRequest, SetPluginEnabledReply, SetPluginEnabledRequest, PLUGIN_TOKEN_METADATA_KEY,
};
use atlas_store::{AuditRow, PluginRow, Store};

use crate::ipc::SharedStore;

/// How long a minted plugin session token is valid, in milliseconds (~5 min).
/// Long enough for a plugin to do real work in a session, short enough that a
/// leaked token is not indefinitely useful.
const SESSION_TTL_MS: i64 = 5 * 60 * 1000;

/// How long a launch nonce is valid before it must be exchanged, in ms (~60 s).
pub const LAUNCH_NONCE_TTL_MS: i64 = 60_000;

/// Wall-clock Unix-epoch milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Capability bitmask helpers. The store persists granted capabilities as a
// bitmask of proto `PluginCapability` discriminants (bit `1 << cap`).
// ---------------------------------------------------------------------------

/// Every grantable (non-`Unspecified`) capability, in discriminant order.
pub const ALL_CAPS: [PluginCapability; 7] = [
    PluginCapability::PluginCapSnapshot,
    PluginCapability::PluginCapHistory,
    PluginCapability::PluginCapSearch,
    PluginCapability::PluginCapIncidents,
    PluginCapability::PluginCapInventory,
    PluginCapability::PluginCapNetwork,
    PluginCapability::PluginCapForensics,
];

/// The single-bit mask for one capability (`1 << discriminant`).
fn cap_bit(cap: PluginCapability) -> i64 {
    1i64 << (cap as i64)
}

/// Whether `mask` grants `cap`.
fn mask_has(mask: i64, cap: PluginCapability) -> bool {
    mask & cap_bit(cap) != 0
}

/// Folds a slice of proto capability discriminants into a bitmask, ignoring
/// unknown / unspecified values.
pub fn caps_to_mask(caps: &[i32]) -> i64 {
    caps.iter()
        .filter_map(|c| PluginCapability::try_from(*c).ok())
        .filter(|c| *c != PluginCapability::Unspecified)
        .map(cap_bit)
        .fold(0, |acc, b| acc | b)
}

/// Expands a bitmask back into the proto capability discriminants it sets.
pub fn mask_to_caps(mask: i64) -> Vec<i32> {
    ALL_CAPS
        .iter()
        .filter(|c| mask_has(mask, **c))
        .map(|c| *c as i32)
        .collect()
}

/// Short human name for a capability (for messages + the CLI).
pub fn cap_name(cap: PluginCapability) -> &'static str {
    match cap {
        PluginCapability::Unspecified => "unspecified",
        PluginCapability::PluginCapSnapshot => "snapshot",
        PluginCapability::PluginCapHistory => "history",
        PluginCapability::PluginCapSearch => "search",
        PluginCapability::PluginCapIncidents => "incidents",
        PluginCapability::PluginCapInventory => "inventory",
        PluginCapability::PluginCapNetwork => "network",
        PluginCapability::PluginCapForensics => "forensics",
    }
}

/// Parses a comma-separated capability list (`snapshot,history,...`) into a
/// bitmask, returning the first unknown token as an error.
pub fn parse_caps(csv: &str) -> Result<i64, String> {
    let mut mask = 0i64;
    for tok in csv.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let cap = ALL_CAPS
            .iter()
            .find(|c| cap_name(**c).eq_ignore_ascii_case(tok))
            .ok_or_else(|| format!("unknown capability '{tok}'"))?;
        mask |= cap_bit(*cap);
    }
    Ok(mask)
}

// ---------------------------------------------------------------------------
// The RPC -> capability map + the pure allow/deny decision. This is the exact
// mapping the interceptor enforces (proto security-model comment); it is
// unit-tested exhaustively below.
// ---------------------------------------------------------------------------

/// Maps a bare AtlasQuery RPC method name to the read-only [`PluginCapability`]
/// group that gates it. `None` means the method is not exposed to plugins at all
/// (either it is a mutation, or it is a read outside the granted surface) — a
/// plugin token can never reach it.
pub fn method_capability(method: &str) -> Option<PluginCapability> {
    Some(match method {
        "GetSnapshot" | "StreamSnapshots" => PluginCapability::PluginCapSnapshot,
        "QueryRange" | "ListEvents" | "ListBookmarks" => PluginCapability::PluginCapHistory,
        "Search" => PluginCapability::PluginCapSearch,
        "ListIncidents" | "Diagnose" => PluginCapability::PluginCapIncidents,
        "ListServices" | "ListStartup" | "ListScheduledTasks" => {
            PluginCapability::PluginCapInventory
        }
        "ListConnections" | "ListListeningPorts" => PluginCapability::PluginCapNetwork,
        "ListSystemChanges" | "ListCrashes" => PluginCapability::PluginCapForensics,
        _ => return None,
    })
}

/// The pure allow/deny decision for a plugin-token'd AtlasQuery call: the called
/// `method` must map to a capability the token was granted. Returns `Ok(())` to
/// allow or `Err(reason)` to deny (the interceptor turns the reason into a
/// `PermissionDenied` status and audits it).
pub fn query_decision(method: &str, granted_mask: i64) -> Result<(), String> {
    match method_capability(method) {
        Some(cap) if mask_has(granted_mask, cap) => Ok(()),
        Some(cap) => Err(format!(
            "plugin was not granted the '{}' capability required for {method}",
            cap_name(cap)
        )),
        None => Err(format!(
            "{method} is not exposed to plugins (no read-only capability maps to it)"
        )),
    }
}

// ---------------------------------------------------------------------------
// In-memory session-token map. Tokens are minted on OpenPluginSession and looked
// up by the interceptor on every plugin-token'd call. Never persisted.
// ---------------------------------------------------------------------------

/// A live plugin session's server-side record.
#[derive(Clone)]
struct SessionRecord {
    plugin_id: i64,
    granted_mask: i64,
    expires_ms: i64,
}

/// The session-token store shared between the [`PluginsService`] (which mints on
/// OpenPluginSession) and the [`PluginGuard`] interceptor (which validates on
/// every call).
pub struct PluginSessions {
    sessions: Mutex<HashMap<String, SessionRecord>>,
    run_nonce: u64,
    counter: AtomicU64,
}

impl Default for PluginSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginSessions {
    pub fn new() -> Self {
        let sessions = Mutex::new(HashMap::new());
        let run_nonce = {
            let mut h = DefaultHasher::new();
            std::process::id().hash(&mut h);
            now_ms().hash(&mut h);
            (&sessions as *const _ as usize).hash(&mut h);
            h.finish()
        };
        Self {
            sessions,
            run_nonce,
            counter: AtomicU64::new(0),
        }
    }

    /// Mints an opaque 128-bit token bound to `plugin_id` (unguessable across
    /// runs via the per-run nonce + monotonic counter, mirroring the broker).
    fn mint_token(&self, plugin_id: i64) -> String {
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut h = DefaultHasher::new();
        self.run_nonce.hash(&mut h);
        seq.hash(&mut h);
        plugin_id.hash(&mut h);
        now_ms().hash(&mut h);
        let a = h.finish();
        let mut h2 = DefaultHasher::new();
        a.hash(&mut h2);
        0x9E37_79B9_7F4A_7C15u64.hash(&mut h2);
        let b = h2.finish();
        format!("{a:016x}{b:016x}")
    }

    /// Opens a session for `plugin_id` scoped to `granted_mask`. Returns the
    /// token and its expiry (Unix ms).
    pub fn open(&self, plugin_id: i64, granted_mask: i64) -> Option<(String, i64)> {
        let token = self.mint_token(plugin_id);
        let expires_ms = now_ms() + SESSION_TTL_MS;
        let mut map = self.sessions.lock().ok()?;
        // Opportunistically drop expired tokens so the map can't grow unbounded.
        let cutoff = now_ms();
        map.retain(|_, r| r.expires_ms >= cutoff);
        map.insert(
            token.clone(),
            SessionRecord {
                plugin_id,
                granted_mask,
                expires_ms,
            },
        );
        Some((token, expires_ms))
    }

    /// Looks up a token, returning its (plugin_id, granted_mask) when present and
    /// unexpired. `None` for an unknown or expired token.
    fn lookup(&self, token: &str) -> Option<(i64, i64)> {
        let map = self.sessions.lock().ok()?;
        let rec = map.get(token)?;
        if now_ms() > rec.expires_ms {
            return None;
        }
        Some((rec.plugin_id, rec.granted_mask))
    }
}

/// Mints a one-time launch nonce (opaque hex). The launcher persists it in the
/// store bound to a plugin id; the plugin presents it to `OpenPluginSession`.
pub fn mint_launch_nonce(plugin_id: i64) -> String {
    let mut h = DefaultHasher::new();
    std::process::id().hash(&mut h);
    now_ms().hash(&mut h);
    plugin_id.hash(&mut h);
    let slot = 0u8;
    (&slot as *const _ as usize).hash(&mut h);
    let a = h.finish();
    let mut h2 = DefaultHasher::new();
    a.hash(&mut h2);
    0xD1B5_4A32_D192_ED03u64.hash(&mut h2);
    let b = h2.finish();
    format!("{a:016x}{b:016x}")
}

// ---------------------------------------------------------------------------
// Registry + signature gate. Shared by the AtlasPlugins RPC service and the
// `plugin` dev CLI so both go through the exact same signature refusal + audit.
// ---------------------------------------------------------------------------

/// Maps a collector [`SignatureStatus`] to the proto [`PluginSignature`]
/// discriminant recorded in the registry.
fn sig_discriminant(status: SignatureStatus) -> i32 {
    match status {
        SignatureStatus::Signed => PluginSignature::PluginSigned as i32,
        SignatureStatus::Unsigned => PluginSignature::PluginUnsigned as i32,
        SignatureStatus::Unknown => PluginSignature::PluginSigUnknown as i32,
    }
}

/// Human label for a stored signature discriminant.
pub fn sig_label(disc: i32) -> &'static str {
    match PluginSignature::try_from(disc) {
        Ok(PluginSignature::PluginSigned) => "SIGNED",
        Ok(PluginSignature::PluginUnsigned) => "UNSIGNED",
        Ok(PluginSignature::PluginSigUnknown) => "UNKNOWN",
        _ => "UNSPECIFIED",
    }
}

/// The outcome of a registration attempt.
pub struct RegisterResult {
    pub ok: bool,
    pub message: String,
    pub row: Option<PluginRow>,
}

/// Writes one plugin audit row (best-effort; a failed write is logged, never
/// blocks the decision — mirrors the broker).
fn audit(store: &Store, action: &str, subject: &str, decision: &str, detail: &str) {
    let row = AuditRow {
        ts_ms: now_ms(),
        actor: "plugin-admin".to_string(),
        action: action.to_string(),
        pid: 0,
        image_name: subject.to_string(),
        decision: decision.to_string(),
        detail: detail.to_string(),
    };
    if let Err(e) = store.record_audit(&row) {
        tracing::warn!("plugin audit write failed: {e}");
    }
}

/// Derives a display name + version from a file's version resource, falling back
/// to the file stem + "0.0.0".
fn name_and_version(exe_path: &str) -> (String, String) {
    let stem = Path::new(exe_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("plugin")
        .to_string();
    match read_version_info(exe_path) {
        Some(v) => {
            let name = if !v.product_name.trim().is_empty() {
                v.product_name
            } else {
                stem
            };
            let version = if !v.file_version.trim().is_empty() {
                v.file_version
            } else if !v.product_version.trim().is_empty() {
                v.product_version
            } else {
                "0.0.0".to_string()
            };
            (name, version)
        }
        None => (stem, "0.0.0".to_string()),
    }
}

/// Registers a plugin executable after verifying its Authenticode signature.
/// REFUSES an unsigned (or unverifiable) executable unless `allow_unsigned` is
/// set. Records the verified publisher and grants `requested_mask`. Audited.
pub fn register_plugin(
    store: &Store,
    exe_path: &str,
    requested_mask: i64,
    allow_unsigned: bool,
) -> anyhow::Result<RegisterResult> {
    if !Path::new(exe_path).is_file() {
        let message = format!("refused: executable not found: {exe_path}");
        audit(
            store,
            "PLUGIN_REGISTER",
            exe_path,
            "REGISTER_DENIED",
            &message,
        );
        return Ok(RegisterResult {
            ok: false,
            message,
            row: None,
        });
    }

    let sig = verify_signature_info(exe_path);
    let disc = sig_discriminant(sig.status);

    if sig.status != SignatureStatus::Signed && !allow_unsigned {
        let message = match sig.status {
            SignatureStatus::Unsigned => "refused: executable is not signed".to_string(),
            _ => "refused: executable signature could not be verified".to_string(),
        };
        audit(
            store,
            "PLUGIN_REGISTER",
            exe_path,
            "REGISTER_DENIED",
            &format!("{message} (signature={})", sig_label(disc)),
        );
        return Ok(RegisterResult {
            ok: false,
            message,
            row: None,
        });
    }

    let (name, version) = name_and_version(exe_path);
    let mut row = PluginRow {
        id: 0,
        name,
        version,
        publisher: sig.publisher.clone(),
        exe_path: exe_path.to_string(),
        signature: disc,
        enabled: false,
        granted_caps: requested_mask,
        registered_ms: now_ms(),
        description: String::new(),
    };
    let id = store.insert_plugin(&row)?;
    row.id = id;
    let publisher = if row.publisher.is_empty() {
        "<none>"
    } else {
        &row.publisher
    };
    let message = format!(
        "registered plugin #{id} '{}' v{} [{}] publisher={} caps=[{}]",
        row.name,
        row.version,
        sig_label(disc),
        publisher,
        mask_to_caps(row.granted_caps)
            .iter()
            .filter_map(|c| PluginCapability::try_from(*c).ok())
            .map(cap_name)
            .collect::<Vec<_>>()
            .join(",")
    );
    audit(
        store,
        "PLUGIN_REGISTER",
        &row.name,
        "REGISTER_OK",
        &format!(
            "#{id} exe={} signature={} publisher={}",
            row.exe_path,
            sig_label(disc),
            publisher
        ),
    );
    Ok(RegisterResult {
        ok: true,
        message,
        row: Some(row),
    })
}

/// Enables or disables a plugin. Enabling RE-VERIFIES the signature and refuses
/// if it degraded from the signed state it had at registration (a signed plugin
/// whose signature is no longer valid must not run). Audited.
pub fn set_enabled(store: &Store, id: i64, enabled: bool) -> anyhow::Result<(bool, String)> {
    let row = match store.get_plugin(id)? {
        Some(r) => r,
        None => return Ok((false, format!("no plugin #{id}"))),
    };

    if enabled {
        let sig = verify_signature_info(&row.exe_path);
        let new_disc = sig_discriminant(sig.status);
        let was_signed = row.signature == PluginSignature::PluginSigned as i32;
        // Record the freshest verdict + publisher regardless.
        store.set_plugin_signature(id, new_disc, &sig.publisher)?;
        if was_signed && sig.status != SignatureStatus::Signed {
            let message = format!(
                "refused: signature degraded since registration (was SIGNED, now {})",
                sig_label(new_disc)
            );
            audit(store, "PLUGIN_ENABLE", &row.name, "ENABLE_DENIED", &message);
            return Ok((false, message));
        }
    }

    let affected = store.set_plugin_enabled(id, enabled)?;
    if !affected {
        return Ok((false, format!("no plugin #{id}")));
    }
    let verb = if enabled { "enabled" } else { "disabled" };
    audit(
        store,
        "PLUGIN_ENABLE",
        &row.name,
        "ENABLE_OK",
        &format!("#{id} {verb}"),
    );
    Ok((true, format!("plugin #{id} {verb}")))
}

/// Replaces a plugin's granted capabilities. Audited.
pub fn grant_capabilities(store: &Store, id: i64, mask: i64) -> anyhow::Result<(bool, String)> {
    let row = match store.get_plugin(id)? {
        Some(r) => r,
        None => return Ok((false, format!("no plugin #{id}"))),
    };
    let affected = store.set_plugin_caps(id, mask)?;
    if !affected {
        return Ok((false, format!("no plugin #{id}")));
    }
    let caps = mask_to_caps(mask)
        .iter()
        .filter_map(|c| PluginCapability::try_from(*c).ok())
        .map(cap_name)
        .collect::<Vec<_>>()
        .join(",");
    audit(
        store,
        "PLUGIN_GRANT",
        &row.name,
        "GRANT_OK",
        &format!("#{id} caps=[{caps}]"),
    );
    Ok((true, format!("plugin #{id} granted caps=[{caps}]")))
}

/// Removes a plugin from the registry. Audited.
pub fn remove_plugin(store: &Store, id: i64) -> anyhow::Result<(bool, String)> {
    let name = store.get_plugin(id)?.map(|r| r.name).unwrap_or_default();
    let affected = store.delete_plugin(id)?;
    if !affected {
        return Ok((false, format!("no plugin #{id}")));
    }
    audit(
        store,
        "PLUGIN_REMOVE",
        &name,
        "REMOVE_OK",
        &format!("#{id}"),
    );
    Ok((true, format!("plugin #{id} removed")))
}

/// Converts a store [`PluginRow`] to the proto [`Plugin`].
pub fn row_to_proto(row: &PluginRow) -> Plugin {
    Plugin {
        id: row.id,
        name: row.name.clone(),
        version: row.version.clone(),
        publisher: row.publisher.clone(),
        exe_path: row.exe_path.clone(),
        signature: row.signature,
        enabled: row.enabled,
        granted: mask_to_caps(row.granted_caps),
        registered_ms: row.registered_ms,
        description: row.description.clone(),
    }
}

// ---------------------------------------------------------------------------
// The AtlasPlugins gRPC service — the registry/management + session-open surface.
// ---------------------------------------------------------------------------

/// The AtlasPlugins service. Shares the query service's store (registry + audit)
/// and owns the in-memory session-token map (shared with the interceptor).
pub struct PluginsService {
    store: SharedStore,
    sessions: Arc<PluginSessions>,
}

impl PluginsService {
    pub fn new(store: SharedStore, sessions: Arc<PluginSessions>) -> Self {
        Self { store, sessions }
    }
}

fn poisoned() -> Status {
    Status::internal("store mutex poisoned")
}

#[tonic::async_trait]
impl AtlasPlugins for PluginsService {
    async fn list_plugins(
        &self,
        _req: Request<ListPluginsRequest>,
    ) -> Result<Response<ListPluginsReply>, Status> {
        let store = self.store.lock().map_err(|_| poisoned())?;
        let rows = store
            .list_plugins()
            .map_err(|e| Status::internal(format!("list_plugins: {e}")))?;
        Ok(Response::new(ListPluginsReply {
            plugins: rows.iter().map(row_to_proto).collect(),
        }))
    }

    async fn register_plugin(
        &self,
        req: Request<RegisterPluginRequest>,
    ) -> Result<Response<RegisterPluginReply>, Status> {
        let r = req.into_inner();
        let mask = caps_to_mask(&r.requested);
        let store = self.store.lock().map_err(|_| poisoned())?;
        let out = register_plugin(&store, &r.exe_path, mask, r.allow_unsigned)
            .map_err(|e| Status::internal(format!("register_plugin: {e}")))?;
        Ok(Response::new(RegisterPluginReply {
            ok: out.ok,
            message: out.message,
            plugin: out.row.as_ref().map(row_to_proto),
        }))
    }

    async fn set_plugin_enabled(
        &self,
        req: Request<SetPluginEnabledRequest>,
    ) -> Result<Response<SetPluginEnabledReply>, Status> {
        let r = req.into_inner();
        let store = self.store.lock().map_err(|_| poisoned())?;
        let (ok, message) = set_enabled(&store, r.id, r.enabled)
            .map_err(|e| Status::internal(format!("set_plugin_enabled: {e}")))?;
        Ok(Response::new(SetPluginEnabledReply { ok, message }))
    }

    async fn grant_plugin_capabilities(
        &self,
        req: Request<GrantPluginCapabilitiesRequest>,
    ) -> Result<Response<GrantPluginCapabilitiesReply>, Status> {
        let r = req.into_inner();
        let mask = caps_to_mask(&r.granted);
        let store = self.store.lock().map_err(|_| poisoned())?;
        let (ok, _message) = grant_capabilities(&store, r.id, mask)
            .map_err(|e| Status::internal(format!("grant_plugin_capabilities: {e}")))?;
        Ok(Response::new(GrantPluginCapabilitiesReply { ok }))
    }

    async fn remove_plugin(
        &self,
        req: Request<RemovePluginRequest>,
    ) -> Result<Response<RemovePluginReply>, Status> {
        let id = req.into_inner().id;
        let store = self.store.lock().map_err(|_| poisoned())?;
        let (ok, _message) = remove_plugin(&store, id)
            .map_err(|e| Status::internal(format!("remove_plugin: {e}")))?;
        Ok(Response::new(RemovePluginReply { ok }))
    }

    async fn open_plugin_session(
        &self,
        req: Request<OpenPluginSessionRequest>,
    ) -> Result<Response<OpenPluginSessionReply>, Status> {
        let r = req.into_inner();
        let store = self.store.lock().map_err(|_| poisoned())?;

        // The nonce must be one the service minted for THIS plugin, unused and
        // unexpired (single-use claim, atomic in the store).
        let claimed = store
            .claim_plugin_nonce(&r.launch_nonce, r.plugin_id, now_ms())
            .map_err(|e| Status::internal(format!("claim_plugin_nonce: {e}")))?;
        if claimed.is_none() {
            audit(
                &store,
                "PLUGIN_SESSION",
                &format!("plugin#{}", r.plugin_id),
                "SESSION_DENIED",
                "invalid, expired, used, or mismatched launch nonce",
            );
            return Ok(Response::new(OpenPluginSessionReply {
                ok: false,
                message: "invalid or expired launch nonce".to_string(),
                session_token: String::new(),
                granted: Vec::new(),
            }));
        }

        let row = match store
            .get_plugin(r.plugin_id)
            .map_err(|e| Status::internal(format!("get_plugin: {e}")))?
        {
            Some(row) => row,
            None => {
                audit(
                    &store,
                    "PLUGIN_SESSION",
                    &format!("plugin#{}", r.plugin_id),
                    "SESSION_DENIED",
                    "plugin not registered",
                );
                return Ok(Response::new(OpenPluginSessionReply {
                    ok: false,
                    message: "plugin not registered".to_string(),
                    session_token: String::new(),
                    granted: Vec::new(),
                }));
            }
        };

        if !row.enabled {
            audit(
                &store,
                "PLUGIN_SESSION",
                &row.name,
                "SESSION_DENIED",
                "plugin is disabled",
            );
            return Ok(Response::new(OpenPluginSessionReply {
                ok: false,
                message: "plugin is disabled".to_string(),
                session_token: String::new(),
                granted: Vec::new(),
            }));
        }

        // Re-verify the signature at session open: a plugin that was signed at
        // registration but whose signature has since degraded must not get a
        // session (defence in depth beyond the enable-time check).
        let sig = verify_signature_info(&row.exe_path);
        let was_signed = row.signature == PluginSignature::PluginSigned as i32;
        if was_signed && sig.status != SignatureStatus::Signed {
            store
                .set_plugin_signature(row.id, sig_discriminant(sig.status), &sig.publisher)
                .ok();
            audit(
                &store,
                "PLUGIN_SESSION",
                &row.name,
                "SESSION_DENIED",
                "signature degraded since registration",
            );
            return Ok(Response::new(OpenPluginSessionReply {
                ok: false,
                message: "plugin signature is no longer valid".to_string(),
                session_token: String::new(),
                granted: Vec::new(),
            }));
        }

        let (token, expires_ms) = match self.sessions.open(row.id, row.granted_caps) {
            Some(t) => t,
            None => return Err(Status::internal("session map poisoned")),
        };
        let granted = mask_to_caps(row.granted_caps);
        audit(
            &store,
            "PLUGIN_SESSION",
            &row.name,
            "SESSION_OPEN",
            &format!(
                "#{} token issued, expires in {}s, caps=[{}]",
                row.id,
                (expires_ms - now_ms()) / 1000,
                granted
                    .iter()
                    .filter_map(|c| PluginCapability::try_from(*c).ok())
                    .map(cap_name)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        );
        Ok(Response::new(OpenPluginSessionReply {
            ok: true,
            message: String::new(),
            session_token: token,
            granted,
        }))
    }
}

// ---------------------------------------------------------------------------
// The interceptor: a tonic/tower service wrapper that enforces the capability
// scope on every request. Added to the router per-service so it sits in front of
// each generated server (and stays a plain `Router` for the transport).
// ---------------------------------------------------------------------------

/// Which surface a [`PluginGuard`] fronts. `Query` is method-aware (maps the RPC
/// to its capability); `Mutating` covers AtlasControl / AtlasRules / AtlasPlugins
/// and rejects ANY plugin token outright — plugins are read-only.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GuardScope {
    Query,
    Mutating,
}

/// The capability interceptor. Wraps a generated tonic server `S` and enforces
/// the plugin-token policy before delegating.
#[derive(Clone)]
pub struct PluginGuard<S> {
    inner: S,
    sessions: Arc<PluginSessions>,
    store: SharedStore,
    scope: GuardScope,
}

impl<S> PluginGuard<S> {
    /// Guards the AtlasQuery read surface (method-aware capability check).
    pub fn query(inner: S, sessions: Arc<PluginSessions>, store: SharedStore) -> Self {
        Self {
            inner,
            sessions,
            store,
            scope: GuardScope::Query,
        }
    }

    /// Guards a mutating / management surface (any plugin token is rejected).
    pub fn mutating(inner: S, sessions: Arc<PluginSessions>, store: SharedStore) -> Self {
        Self {
            inner,
            sessions,
            store,
            scope: GuardScope::Mutating,
        }
    }

    /// Audits a rejected plugin call (part of the non-negotiable audit trail).
    fn audit_reject(&self, method: &str, plugin_id: i64, reason: &str) {
        if let Ok(store) = self.store.lock() {
            let subject = if plugin_id > 0 {
                format!("plugin#{plugin_id}")
            } else {
                "plugin".to_string()
            };
            audit(&store, method, &subject, "PLUGIN_REJECTED", reason);
        }
    }

    /// The full policy decision for one request. `None` allows (delegate to the
    /// inner service); `Some(status)` denies (and has already been audited).
    fn decide(&self, method: &str, token: Option<&str>) -> Option<Status> {
        let token = token?; // no token → first-party UI / CLI: full read access
        match self.scope {
            GuardScope::Mutating => {
                self.audit_reject(
                    method,
                    0,
                    "plugin token presented to a mutating/management surface",
                );
                Some(Status::permission_denied(format!(
                    "{method} is not available to plugins (read-only)"
                )))
            }
            GuardScope::Query => match self.sessions.lookup(token) {
                None => {
                    self.audit_reject(method, 0, "invalid or expired session token");
                    Some(Status::unauthenticated(
                        "invalid or expired plugin session token",
                    ))
                }
                Some((plugin_id, granted_mask)) => match query_decision(method, granted_mask) {
                    Ok(()) => None,
                    Err(reason) => {
                        self.audit_reject(method, plugin_id, &reason);
                        Some(Status::permission_denied(reason))
                    }
                },
            },
        }
    }
}

impl<S> NamedService for PluginGuard<S>
where
    S: NamedService,
{
    const NAME: &'static str = S::NAME;
}

impl<S> Service<http::Request<Body>> for PluginGuard<S>
where
    S: Service<http::Request<Body>, Response = http::Response<Body>> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = http::Response<Body>;
    type Error = S::Error;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: http::Request<Body>) -> Self::Future {
        // The gRPC method is the last path segment: "/atlas.v0.AtlasQuery/Search".
        let method = req
            .uri()
            .path()
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        let token = req
            .headers()
            .get(PLUGIN_TOKEN_METADATA_KEY)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);

        match self.decide(&method, token.as_deref()) {
            None => {
                let fut = self.inner.call(req);
                Box::pin(fut)
            }
            Some(status) => Box::pin(async move { Ok(status.into_http()) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Store {
        Store::open_in_memory().unwrap()
    }

    #[test]
    fn caps_mask_round_trips() {
        let mask = caps_to_mask(&[
            PluginCapability::PluginCapSnapshot as i32,
            PluginCapability::PluginCapNetwork as i32,
        ]);
        let mut got = mask_to_caps(mask);
        got.sort();
        let mut want = vec![
            PluginCapability::PluginCapSnapshot as i32,
            PluginCapability::PluginCapNetwork as i32,
        ];
        want.sort();
        assert_eq!(got, want);
        // Unspecified is never grantable.
        assert_eq!(caps_to_mask(&[PluginCapability::Unspecified as i32]), 0);
    }

    #[test]
    fn parse_caps_reads_names() {
        let mask = parse_caps("snapshot, history , forensics").unwrap();
        assert!(mask_has(mask, PluginCapability::PluginCapSnapshot));
        assert!(mask_has(mask, PluginCapability::PluginCapHistory));
        assert!(mask_has(mask, PluginCapability::PluginCapForensics));
        assert!(!mask_has(mask, PluginCapability::PluginCapSearch));
        assert!(parse_caps("snapshot,bogus").is_err());
    }

    /// The RPC->capability map covers exactly the read RPCs named in the proto
    /// security-model comment and NOTHING else.
    #[test]
    fn method_capability_map_is_exact() {
        use PluginCapability::*;
        let cases = [
            ("GetSnapshot", Some(PluginCapSnapshot)),
            ("StreamSnapshots", Some(PluginCapSnapshot)),
            ("QueryRange", Some(PluginCapHistory)),
            ("ListEvents", Some(PluginCapHistory)),
            ("ListBookmarks", Some(PluginCapHistory)),
            ("Search", Some(PluginCapSearch)),
            ("ListIncidents", Some(PluginCapIncidents)),
            ("Diagnose", Some(PluginCapIncidents)),
            ("ListServices", Some(PluginCapInventory)),
            ("ListStartup", Some(PluginCapInventory)),
            ("ListScheduledTasks", Some(PluginCapInventory)),
            ("ListConnections", Some(PluginCapNetwork)),
            ("ListListeningPorts", Some(PluginCapNetwork)),
            ("ListSystemChanges", Some(PluginCapForensics)),
            ("ListCrashes", Some(PluginCapForensics)),
        ];
        for (m, want) in cases {
            assert_eq!(method_capability(m), want, "method {m}");
        }
        // Mutations + reads outside the plugin surface map to nothing.
        for m in [
            "CreateBookmark",
            "GetCapabilities",
            "GenerateReport",
            "GetProcessDetail",
            "ListHandles",
            "CreatePrivacyAlertRule",
            "GetBatteryStatus",
            "GenerateSupportBundle",
        ] {
            assert_eq!(method_capability(m), None, "method {m} must be unmapped");
        }
    }

    /// Exhaustive allow/deny of the pure decision: a granted method is allowed;
    /// an ungranted-but-mapped method is denied; an unmapped method (mutation or
    /// off-surface read) is denied even with every capability granted.
    #[test]
    fn query_decision_allows_only_granted() {
        let snapshot_only = caps_to_mask(&[PluginCapability::PluginCapSnapshot as i32]);
        // Granted -> allow.
        assert!(query_decision("GetSnapshot", snapshot_only).is_ok());
        assert!(query_decision("StreamSnapshots", snapshot_only).is_ok());
        // Mapped but not granted -> deny.
        assert!(query_decision("Search", snapshot_only).is_err());
        assert!(query_decision("ListConnections", snapshot_only).is_err());

        // Every capability granted still cannot reach an unmapped method.
        let all = ALL_CAPS.iter().map(|c| cap_bit(*c)).fold(0, |a, b| a | b);
        assert!(query_decision("GetSnapshot", all).is_ok());
        assert!(query_decision("ListCrashes", all).is_ok());
        assert!(query_decision("CreateBookmark", all).is_err()); // mutation
        assert!(query_decision("GetProcessDetail", all).is_err()); // off-surface read
        assert!(query_decision("PrepareAction", all).is_err()); // not an AtlasQuery read
    }

    #[test]
    fn sessions_mint_lookup_and_scope() {
        let s = PluginSessions::new();
        let mask = caps_to_mask(&[PluginCapability::PluginCapSnapshot as i32]);
        let (token, _exp) = s.open(42, mask).unwrap();
        let (pid, got_mask) = s.lookup(&token).unwrap();
        assert_eq!(pid, 42);
        assert_eq!(got_mask, mask);
        // Unknown token resolves to nothing.
        assert!(s.lookup("deadbeef").is_none());
    }

    /// The guard's policy decision, end to end, over the store-backed session
    /// map: no token -> allow; valid token on a granted method -> allow;
    /// ungranted / mutation with token -> deny; and a Mutating-scope guard denies
    /// ANY token regardless of grants.
    #[test]
    fn guard_decision_matrix() {
        let store: SharedStore = Arc::new(Mutex::new(mem()));
        let sessions = Arc::new(PluginSessions::new());
        let mask = caps_to_mask(&[PluginCapability::PluginCapSnapshot as i32]);
        let (token, _e) = sessions.open(7, mask).unwrap();

        // A dummy inner service is never invoked by `decide` (it only returns the
        // allow/deny verdict), so we can test the policy without a real service.
        let q = PluginGuard::query((), sessions.clone(), store.clone());
        let m = PluginGuard::mutating((), sessions.clone(), store.clone());

        // No token -> first-party allow on both surfaces.
        assert!(q.decide("GetSnapshot", None).is_none());
        assert!(m.decide("CreateRule", None).is_none());

        // Query surface: granted allow, ungranted deny, mutation deny, bad token deny.
        assert!(q.decide("GetSnapshot", Some(&token)).is_none());
        assert!(q.decide("Search", Some(&token)).is_some());
        assert!(q.decide("CreateBookmark", Some(&token)).is_some());
        assert!(q.decide("GetSnapshot", Some("not-a-token")).is_some());

        // Mutating surface: ANY token denied, even a valid one.
        assert!(m.decide("ListRules", Some(&token)).is_some());
        assert!(m.decide("PrepareAction", Some(&token)).is_some());
        assert!(m.decide("RegisterPlugin", Some(&token)).is_some());
    }

    /// Unsigned executables are refused unless explicitly overridden. Uses this
    /// test binary itself as an on-disk unsigned image.
    #[test]
    fn register_refuses_unsigned() {
        let store = mem();
        let exe = std::env::current_exe().unwrap();
        let exe = exe.to_str().unwrap();
        let sig = verify_signature_info(exe);
        // The cargo test binary is not Authenticode-signed.
        if sig.status == SignatureStatus::Signed {
            return; // environment-dependent; skip if somehow signed
        }
        let refused = register_plugin(&store, exe, 0, false).unwrap();
        assert!(!refused.ok);
        assert!(refused.message.contains("refused"));
        assert!(store.list_plugins().unwrap().is_empty());

        // With the override it registers (disabled, UNSIGNED recorded).
        let ok = register_plugin(&store, exe, 0, true).unwrap();
        assert!(ok.ok, "allow_unsigned should register: {}", ok.message);
        let row = ok.row.unwrap();
        assert!(!row.enabled);
        assert_ne!(row.signature, PluginSignature::PluginSigned as i32);
        // The refusal + the override were both audited.
        let audit = store.recent_audit(10).unwrap();
        assert!(audit.iter().any(|a| a.decision == "REGISTER_DENIED"));
        assert!(audit.iter().any(|a| a.decision == "REGISTER_OK"));
    }
}
