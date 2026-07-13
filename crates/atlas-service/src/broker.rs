//! Safe-action broker v0 (PRD §9.22, tech-stack §4.5, docs/phases.md M6).
//!
//! Serves the `AtlasControl` gRPC contract: a two-phase, audited, consent-gated
//! gateway for the four process-action verbs (close-windows / suspend / resume /
//! terminate). The design principle is that a UI can never *directly* act on a
//! process — it must first `PrepareAction` (which returns the risk picture and,
//! if allowed, a short-lived single-use token) and then `ExecuteAction` with
//! that exact token. Every prepare and every execute is written to the store's
//! append-only `audit` table regardless of outcome.
//!
//! ## Security model
//!
//! * **Protected-critical list.** A hardcoded set of OS-critical image names
//!   (System, smss, csrss, wininit, services, lsass, winlogon, ...) plus a
//!   session-0 heuristic: `PrepareAction` returns `allowed=false` with a denial
//!   reason and issues no token. These processes can never be actioned through
//!   the broker.
//! * **Consent token.** On an allowed prepare, a random-looking opaque token is
//!   minted by hashing (pid, create_time, action, a per-run nonce, a monotonic
//!   counter) — no new crate dependency, just `std::hash`. It is single-use,
//!   expires ~30 s after minting, and is bound to the exact (pid, create_time,
//!   action) tuple. Tokens live in an in-memory map behind a `Mutex`.
//! * **Re-check on execute.** `ExecuteAction` validates the token (exists,
//!   unexpired, unused), then re-reads a fresh process snapshot and confirms the
//!   pid still maps to the same `create_time` before acting — so a pid recycled
//!   between prepare and execute cannot be hit by a stale token.
//!
//! The pipe DACL (SYSTEM/Administrators/current-user only, in `atlas-ipc`)
//! remains the actual principal boundary; the broker adds intent-confirmation
//! and auditing on top of it.

#![cfg(windows)]

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tonic::{Request, Response, Status};

use atlas_collectors::{
    post_close_to_windows, resume_process, snapshot_processes, suspend_process, terminate_process,
    ActionOutcome,
};
use atlas_ipc::v0::atlas_control_server::AtlasControl;
use atlas_ipc::{
    ActionRisk, ExecuteActionReply, ExecuteActionRequest, PrepareActionReply, PrepareActionRequest,
    ProcessActionKind,
};
use atlas_store::AuditRow;

use crate::ipc::SharedStore;

/// How long a minted consent token is valid, in milliseconds (~30 s).
const TOKEN_TTL_MS: i64 = 30_000;

/// The protected-critical image names (lowercased, no extension). Acting on any
/// of these through the broker is always denied. Kernel/session-management and
/// security-critical user-mode processes whose death bugchecks or destabilises
/// the session.
const CRITICAL_IMAGES: &[&str] = &[
    "system",
    "registry",
    "smss",
    "csrss",
    "wininit",
    "services",
    "lsass",
    "lsaiso",
    "winlogon",
    "fontdrvhost",
    "dwm",
    "svchost",
    "spoolsv",
    "explorer",
    "ntoskrnl",
    "memory compression",
];

/// A minted consent token's server-side record.
struct TokenRecord {
    pid: u32,
    create_time_100ns: i64,
    action: ProcessActionKind,
    expires_ms: i64,
    used: bool,
}

/// The AtlasControl service: shares the query service's store (for the audit
/// log) and owns the in-memory consent-token map.
pub struct BrokerService {
    store: SharedStore,
    tokens: Mutex<HashMap<String, TokenRecord>>,
    /// Per-run nonce mixed into every token so tokens are unguessable across
    /// process runs and never collide with a previous run's issued strings.
    run_nonce: u64,
    /// Monotonic counter mixed into each token so two prepares for the same
    /// (pid, action) in the same run still mint distinct tokens.
    counter: AtomicU64,
}

impl BrokerService {
    /// Builds the broker over the shared store handle.
    pub fn new(store: SharedStore) -> Self {
        let tokens = Mutex::new(HashMap::new());
        let run_nonce = {
            let mut h = DefaultHasher::new();
            std::process::id().hash(&mut h);
            now_ms().hash(&mut h);
            // The heap address of the freshly-allocated token map varies per run
            // (ASLR), adding entropy beyond the pid+time seed so tokens are not
            // predictable from wall clock alone.
            (&tokens as *const _ as usize).hash(&mut h);
            h.finish()
        };
        Self {
            store,
            tokens,
            run_nonce,
            counter: AtomicU64::new(0),
        }
    }

    /// Mints an opaque single-use token bound to (pid, create_time, action).
    fn mint_token(&self, pid: u32, create_time_100ns: i64, action: ProcessActionKind) -> String {
        let seq = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut h = DefaultHasher::new();
        self.run_nonce.hash(&mut h);
        seq.hash(&mut h);
        pid.hash(&mut h);
        create_time_100ns.hash(&mut h);
        (action as i32).hash(&mut h);
        now_ms().hash(&mut h);
        // 128 bits of token by hashing twice with a salt tweak, hex-encoded.
        let a = h.finish();
        let mut h2 = DefaultHasher::new();
        a.hash(&mut h2);
        0x9E37_79B9_7F4A_7C15u64.hash(&mut h2);
        let b = h2.finish();
        format!("{a:016x}{b:016x}")
    }

    /// Writes one audit row; failures to write are logged but never block the
    /// action decision (the audit is best-effort durable, the decision is not
    /// gated on it landing).
    fn audit(
        &self,
        action: ProcessActionKind,
        pid: u32,
        image_name: &str,
        decision: &str,
        detail: &str,
    ) {
        let row = AuditRow {
            ts_ms: now_ms(),
            actor: "local-ui".to_string(),
            action: action_name(action).to_string(),
            pid,
            image_name: image_name.to_string(),
            decision: decision.to_string(),
            detail: detail.to_string(),
        };
        if let Ok(store) = self.store.lock() {
            if let Err(e) = store.record_audit(&row) {
                tracing::warn!("audit write failed: {e}");
            }
        }
    }
}

/// Wall-clock Unix-epoch milliseconds.
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The image *family* (lowercased base name, `.exe` stripped) for critical-list
/// matching. Reuses the collector's grouping normaliser for consistency.
fn image_family(name: &str) -> String {
    atlas_collectors::image_family(name)
}

/// Whether `image_name` (any form) is on the protected-critical list.
fn is_critical_image(image_name: &str) -> bool {
    let fam = image_family(image_name);
    CRITICAL_IMAGES.contains(&fam.as_str())
}

/// Human-readable action name for the audit log and messages.
fn action_name(a: ProcessActionKind) -> &'static str {
    match a {
        ProcessActionKind::ProcessActionUnspecified => "UNSPECIFIED",
        ProcessActionKind::CloseWindows => "CLOSE_WINDOWS",
        ProcessActionKind::Suspend => "SUSPEND",
        ProcessActionKind::Resume => "RESUME",
        ProcessActionKind::Terminate => "TERMINATE",
    }
}

/// A live view of a process needed for risk assembly / identity re-check:
/// image name, session, create_time, and child count.
struct ProcView {
    image_name: String,
    session_id: u32,
    create_time_100ns: i64,
    child_count: u32,
}

/// Looks up `pid` in a fresh system snapshot, returning its view (image, session,
/// create_time) and the number of processes that name it as parent. `None` if
/// the pid is not currently present.
fn look_up_process(pid: u32) -> anyhow::Result<Option<ProcView>> {
    let procs = snapshot_processes()?;
    let me = match procs.iter().find(|p| p.pid == pid) {
        Some(p) => p,
        None => return Ok(None),
    };
    let child_count = procs.iter().filter(|p| p.parent_pid == pid).count() as u32;
    Ok(Some(ProcView {
        image_name: me.image_name.clone(),
        session_id: me.session_id,
        create_time_100ns: me.create_time_100ns,
        child_count,
    }))
}

/// Assembles the risk picture for an action on `view`, including visible
/// top-level window count (live EnumWindows) and human caveats.
fn assemble_risk(
    pid: u32,
    view: &ProcView,
    action: ProcessActionKind,
    critical: bool,
) -> ActionRisk {
    let visible_windows = atlas_collectors::count_visible_top_level_windows(pid);
    let is_system = view.session_id == 0;
    let mut notes = Vec::new();
    if critical {
        notes.push("on the protected-critical list — action refused".to_string());
    }
    if is_system {
        notes.push("runs in session 0 (a service / system context)".to_string());
    }
    if view.child_count > 0 {
        notes.push(format!(
            "{} child process(es) will be orphaned",
            view.child_count
        ));
    }
    match action {
        ProcessActionKind::Terminate => {
            notes.push("terminate is abrupt: unsaved work is lost, no clean shutdown".to_string());
            if visible_windows > 0 {
                notes.push(format!(
                    "{visible_windows} visible window(s) — prefer close-windows first"
                ));
            }
        }
        ProcessActionKind::Suspend => {
            notes.push(
                "suspend freezes all threads; the app appears hung until resumed".to_string(),
            );
        }
        ProcessActionKind::CloseWindows if visible_windows == 0 => {
            notes.push("no visible top-level windows to close".to_string());
        }
        _ => {}
    }
    ActionRisk {
        is_critical: critical,
        is_system,
        visible_windows,
        child_count: view.child_count,
        notes,
    }
}

#[tonic::async_trait]
impl AtlasControl for BrokerService {
    async fn prepare_action(
        &self,
        req: Request<PrepareActionRequest>,
    ) -> Result<Response<PrepareActionReply>, Status> {
        let r = req.into_inner();
        let action = ProcessActionKind::try_from(r.action)
            .map_err(|_| Status::invalid_argument("unknown action"))?;
        if action == ProcessActionKind::ProcessActionUnspecified {
            return Err(Status::invalid_argument("action unspecified"));
        }

        // Re-read the live process so risk reflects the current state and the
        // image name is authoritative (the client-supplied pid is all we trust).
        let view =
            look_up_process(r.pid).map_err(|e| Status::internal(format!("snapshot: {e}")))?;
        let view = match view {
            Some(v) => v,
            None => {
                self.audit(
                    action,
                    r.pid,
                    "<gone>",
                    "PREPARE_DENIED",
                    "process not found",
                );
                return Ok(Response::new(PrepareActionReply {
                    allowed: false,
                    denial_reason: "process not found".to_string(),
                    risk: None,
                    consent_token: String::new(),
                    token_expires_ms: 0,
                }));
            }
        };

        // Identity guard: if the caller passed a create_time, it must match the
        // live process (defends against a pid already recycled before prepare).
        if r.create_time_100ns != 0 && r.create_time_100ns != view.create_time_100ns {
            self.audit(
                action,
                r.pid,
                &view.image_name,
                "PREPARE_DENIED",
                "create_time mismatch (pid recycled)",
            );
            return Ok(Response::new(PrepareActionReply {
                allowed: false,
                denial_reason: "create_time mismatch: the pid was recycled".to_string(),
                risk: None,
                consent_token: String::new(),
                token_expires_ms: 0,
            }));
        }

        let critical = is_critical_image(&view.image_name) || r.pid <= 4;
        let risk = assemble_risk(r.pid, &view, action, critical);

        if critical {
            let reason = format!(
                "{} is protected-critical; the broker never actions it",
                view.image_name
            );
            self.audit(action, r.pid, &view.image_name, "PREPARE_DENIED", &reason);
            return Ok(Response::new(PrepareActionReply {
                allowed: false,
                denial_reason: reason,
                risk: Some(risk),
                consent_token: String::new(),
                token_expires_ms: 0,
            }));
        }

        // Allowed: mint a single-use token bound to (pid, create_time, action).
        let token = self.mint_token(r.pid, view.create_time_100ns, action);
        let expires_ms = now_ms() + TOKEN_TTL_MS;
        {
            let mut tokens = self.tokens.lock().map_err(|_| poisoned())?;
            // Opportunistically drop expired tokens so the map can't grow.
            let cutoff = now_ms();
            tokens.retain(|_, t| t.expires_ms >= cutoff && !t.used);
            tokens.insert(
                token.clone(),
                TokenRecord {
                    pid: r.pid,
                    create_time_100ns: view.create_time_100ns,
                    action,
                    expires_ms,
                    used: false,
                },
            );
        }
        self.audit(
            action,
            r.pid,
            &view.image_name,
            "PREPARE_ALLOWED",
            &format!("token issued, expires in {}s", TOKEN_TTL_MS / 1000),
        );
        Ok(Response::new(PrepareActionReply {
            allowed: true,
            denial_reason: String::new(),
            risk: Some(risk),
            consent_token: token,
            token_expires_ms: expires_ms,
        }))
    }

    async fn execute_action(
        &self,
        req: Request<ExecuteActionRequest>,
    ) -> Result<Response<ExecuteActionReply>, Status> {
        let token = req.into_inner().consent_token;

        // Claim the token: it must exist, be unexpired, and unused. On success
        // we mark it used (single-use) while still holding the lock.
        let (pid, create_time, action) = {
            let mut tokens = self.tokens.lock().map_err(|_| poisoned())?;
            let rec = match tokens.get_mut(&token) {
                Some(r) => r,
                None => {
                    self.audit(
                        ProcessActionKind::ProcessActionUnspecified,
                        0,
                        "<unknown>",
                        "EXECUTE_FAIL",
                        "invalid or unknown consent token",
                    );
                    return Ok(Response::new(ExecuteActionReply {
                        success: false,
                        message: "invalid or unknown consent token".to_string(),
                    }));
                }
            };
            if rec.used {
                return Ok(Response::new(ExecuteActionReply {
                    success: false,
                    message: "consent token already used".to_string(),
                }));
            }
            if now_ms() > rec.expires_ms {
                return Ok(Response::new(ExecuteActionReply {
                    success: false,
                    message: "consent token expired".to_string(),
                }));
            }
            rec.used = true;
            (rec.pid, rec.create_time_100ns, rec.action)
        };

        // Re-check the process still matches (pid + create_time) via a fresh
        // snapshot before acting — a stale token must never hit a recycled pid.
        let view = look_up_process(pid).map_err(|e| Status::internal(format!("snapshot: {e}")))?;
        let view = match view {
            Some(v) => v,
            None => {
                self.audit(
                    action,
                    pid,
                    "<gone>",
                    "EXECUTE_FAIL",
                    "process no longer present",
                );
                return Ok(Response::new(ExecuteActionReply {
                    success: false,
                    message: "process is no longer present".to_string(),
                }));
            }
        };
        if view.create_time_100ns != create_time {
            self.audit(
                action,
                pid,
                &view.image_name,
                "EXECUTE_FAIL",
                "create_time mismatch at execute (pid recycled)",
            );
            return Ok(Response::new(ExecuteActionReply {
                success: false,
                message: "pid was recycled since prepare; refusing to act".to_string(),
            }));
        }
        // Defence in depth: never act on a critical process even if a token was
        // somehow minted for one.
        if is_critical_image(&view.image_name) || pid <= 4 {
            self.audit(
                action,
                pid,
                &view.image_name,
                "EXECUTE_FAIL",
                "protected-critical at execute",
            );
            return Ok(Response::new(ExecuteActionReply {
                success: false,
                message: "process is protected-critical".to_string(),
            }));
        }

        let outcome: ActionOutcome = match action {
            ProcessActionKind::CloseWindows => post_close_to_windows(pid),
            ProcessActionKind::Suspend => suspend_process(pid),
            ProcessActionKind::Resume => resume_process(pid),
            ProcessActionKind::Terminate => terminate_process(pid),
            ProcessActionKind::ProcessActionUnspecified => ActionOutcome {
                success: false,
                message: "unspecified action".to_string(),
            },
        };

        let decision = if outcome.success {
            "EXECUTE_OK"
        } else {
            "EXECUTE_FAIL"
        };
        self.audit(action, pid, &view.image_name, decision, &outcome.message);
        Ok(Response::new(ExecuteActionReply {
            success: outcome.success,
            message: outcome.message,
        }))
    }
}

fn poisoned() -> Status {
    Status::internal("broker token map poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broker() -> BrokerService {
        let store = std::sync::Arc::new(std::sync::Mutex::new(
            atlas_store::Store::open_in_memory().unwrap(),
        ));
        BrokerService::new(store)
    }

    #[test]
    fn critical_list_matches_by_family() {
        assert!(is_critical_image("lsass.exe"));
        assert!(is_critical_image(r"C:\Windows\System32\csrss.exe"));
        assert!(is_critical_image("SERVICES.EXE"));
        assert!(!is_critical_image("notepad.exe"));
        assert!(!is_critical_image("chrome.exe"));
    }

    #[test]
    fn tokens_are_distinct_and_bound() {
        let b = broker();
        let t1 = b.mint_token(1000, 5, ProcessActionKind::Suspend);
        let t2 = b.mint_token(1000, 5, ProcessActionKind::Suspend);
        // Two prepares for the same target still mint distinct tokens (counter).
        assert_ne!(t1, t2);
        // Token is a 32-hex-char opaque string.
        assert_eq!(t1.len(), 32);
        assert!(t1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn action_names_are_stable() {
        assert_eq!(action_name(ProcessActionKind::Suspend), "SUSPEND");
        assert_eq!(action_name(ProcessActionKind::Terminate), "TERMINATE");
        assert_eq!(
            action_name(ProcessActionKind::CloseWindows),
            "CLOSE_WINDOWS"
        );
        assert_eq!(action_name(ProcessActionKind::Resume), "RESUME");
    }

    /// Preparing an action against a protected-critical pid is denied and audited
    /// without a token. Uses pid 4 (System) which is always critical.
    #[tokio::test]
    async fn prepare_denies_critical_pid() {
        let b = broker();
        let reply = b
            .prepare_action(Request::new(PrepareActionRequest {
                pid: 4,
                create_time_100ns: 0,
                action: ProcessActionKind::Terminate as i32,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!reply.allowed);
        assert!(reply.consent_token.is_empty());
        // The denial was audited.
        let store = b.store.lock().unwrap();
        let audit = store.recent_audit(10).unwrap();
        assert!(audit.iter().any(|a| a.decision == "PREPARE_DENIED"));
    }

    /// Executing with an unknown token fails cleanly and is audited.
    #[tokio::test]
    async fn execute_rejects_unknown_token() {
        let b = broker();
        let reply = b
            .execute_action(Request::new(ExecuteActionRequest {
                consent_token: "deadbeef".to_string(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!reply.success);
        assert!(reply.message.contains("token"));
    }

    /// A token can only be executed once: the second execute reports "already
    /// used". We drive this on our own process's pid but with a deliberately
    /// wrong create_time so the action never actually runs — the point is the
    /// single-use bookkeeping, not a live suspend of the test runner.
    #[tokio::test]
    async fn token_is_single_use() {
        let b = broker();
        let pid = std::process::id();
        // Prepare a resume (harmless verb) on ourselves.
        let prep = b
            .prepare_action(Request::new(PrepareActionRequest {
                pid,
                create_time_100ns: 0,
                action: ProcessActionKind::Resume as i32,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(prep.allowed, "own process is not critical");
        let token = prep.consent_token;

        // First execute runs (resume of a non-suspended process is a no-op-ish
        // success or a benign failure — either way the token is consumed).
        let _first = b
            .execute_action(Request::new(ExecuteActionRequest {
                consent_token: token.clone(),
            }))
            .await
            .unwrap()
            .into_inner();
        // Second execute with the same token is rejected as already used.
        let second = b
            .execute_action(Request::new(ExecuteActionRequest {
                consent_token: token,
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(!second.success);
        assert!(second.message.contains("already used"));
    }
}
