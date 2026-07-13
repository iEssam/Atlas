//! Advanced privacy-alerts evaluator (R2, PRD §9.10.3).
//!
//! Consumes [`atlas_collectors::PrivacyTransition`]s from the ConsentStore
//! change-watcher and evaluates the enabled alert rules against each one,
//! recording a [`atlas_store::FiredAlertRow`] for every match and persisting the
//! transition itself to the `privacy_event` history table.
//!
//! The condition matcher is a pure function ([`condition_matches`]) tested
//! against synthetic inputs; the stateful [`Evaluator`] driver holds only the
//! set of currently-active uses (for the duration-based `ALERT_LONGER_THAN`
//! condition) plus a per-session dedup set.
//!
//! Language is deliberately **factual, never accusatory** (PRD §9.10.3): a fired
//! alert says "microphone used in the background", not "spyware". The rules
//! surface *what Windows recorded*, correlation only — never intent.

use atlas_ipc::PrivacyAlertCondition;

/// Proto `PrivacyAlertCondition` discriminants, named for readable matching.
const COND_ANY_USE: i32 = PrivacyAlertCondition::AlertAnyUse as i32;
const COND_BACKGROUND_USE: i32 = PrivacyAlertCondition::AlertBackgroundUse as i32;
const COND_WHILE_LOCKED: i32 = PrivacyAlertCondition::AlertWhileLocked as i32;
const COND_UNKNOWN_APP: i32 = PrivacyAlertCondition::AlertUnknownApp as i32;
const COND_LONGER_THAN: i32 = PrivacyAlertCondition::AlertLongerThan as i32;

/// The evaluator's view of one transition (or a periodic duration check) against
/// which a rule's condition is tested. Kept independent of the collector /
/// proto types so the matcher is pure and cheap to unit-test.
#[derive(Debug, Clone)]
pub struct AlertInput {
    /// Proto `CapabilityKind` discriminant (1=camera, 2=microphone, 3=location).
    pub capability: i32,
    pub app_id: String,
    pub display_name: String,
    /// True when this input represents a capability *start* (the trigger for the
    /// discrete conditions). False for a stop or a periodic duration check.
    pub started: bool,
    /// The app owned the foreground window when observed (best-effort).
    pub foreground: bool,
    /// The interactive session was locked when observed (best-effort).
    pub session_locked: bool,
    /// The app is unsigned / unknown (desktop apps only; packaged ⇒ false).
    pub unknown_app: bool,
    /// Seconds the capability has been continuously active — only meaningful for
    /// a duration check (`ALERT_LONGER_THAN`).
    pub active_seconds: u32,
    /// True when this is a periodic duration sweep rather than a transition; only
    /// `ALERT_LONGER_THAN` fires on these.
    pub duration_check: bool,
    pub ts_ms: i64,
}

/// Whether a rule scoped to `rule_capability` (0 = all) applies to a transition
/// on `ev_capability`.
pub fn capability_applies(rule_capability: i32, ev_capability: i32) -> bool {
    rule_capability == 0 || rule_capability == ev_capability
}

/// The core pure condition test: does `condition` (with `threshold_seconds` for
/// `ALERT_LONGER_THAN`) match `ev`? Discrete conditions fire only on a start
/// transition; `ALERT_LONGER_THAN` fires only on a duration check that has
/// crossed a positive threshold. UNSPECIFIED / unknown conditions never fire.
pub fn condition_matches(
    rule_capability: i32,
    condition: i32,
    threshold_seconds: u32,
    ev: &AlertInput,
) -> bool {
    if !capability_applies(rule_capability, ev.capability) {
        return false;
    }
    match condition {
        COND_ANY_USE => ev.started && !ev.duration_check,
        COND_BACKGROUND_USE => ev.started && !ev.duration_check && !ev.foreground,
        COND_WHILE_LOCKED => ev.started && !ev.duration_check && ev.session_locked,
        COND_UNKNOWN_APP => ev.started && !ev.duration_check && ev.unknown_app,
        COND_LONGER_THAN => {
            ev.duration_check && threshold_seconds > 0 && ev.active_seconds >= threshold_seconds
        }
        _ => false,
    }
}

/// Human capability label for a detail string.
pub fn capability_label(capability: i32) -> &'static str {
    match capability {
        1 => "camera",
        2 => "microphone",
        3 => "location",
        _ => "capability",
    }
}

/// The factual, never-accusatory detail string recorded on a fired alert. States
/// only what Windows recorded — the capability, the observed context, and (for
/// duration) the measured time. No claim of intent.
pub fn alert_detail(condition: i32, threshold_seconds: u32, ev: &AlertInput) -> String {
    let cap = capability_label(ev.capability);
    match condition {
        COND_ANY_USE => format!("{cap} used by {}", ev.display_name),
        COND_BACKGROUND_USE => {
            format!(
                "{cap} used by {} while not in the foreground",
                ev.display_name
            )
        }
        COND_WHILE_LOCKED => {
            format!(
                "{cap} used by {} while the session was locked",
                ev.display_name
            )
        }
        COND_UNKNOWN_APP => format!(
            "{cap} used by {}, an unsigned or unrecognized app",
            ev.display_name
        ),
        COND_LONGER_THAN => format!(
            "{cap} used by {} for {}s (over the {}s threshold)",
            ev.display_name, ev.active_seconds, threshold_seconds
        ),
        _ => format!("{cap} used by {}", ev.display_name),
    }
}

/// Whether an app should be treated as unknown for `ALERT_UNKNOWN_APP`. Packaged
/// (Store) apps are signed by Windows, so they are never "unknown"; a desktop app
/// is unknown when its on-disk image is not trusted-signed. Pure over `signed`
/// so it is unit-testable without the signature FFI.
pub fn is_unknown_app(signed: bool, packaged: bool) -> bool {
    !packaged && !signed
}

#[cfg(windows)]
mod driver {
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::sync::Arc;
    use std::time::Duration;

    use atlas_collectors::SignatureStatus;
    use atlas_collectors::{unmunge_nonpackaged, verify_signature, Capability, PrivacyTransition};
    use atlas_store::{FiredAlertRow, PrivacyEventRow};

    use super::{alert_detail, condition_matches, is_unknown_app, AlertInput};
    use crate::ipc::SharedStore;

    /// One currently-active capability use, tracked for `ALERT_LONGER_THAN`.
    struct ActiveUse {
        display_name: String,
        app_id: String,
        started_ms: i64,
    }

    /// The stateful evaluator driver: owns the active-use table and the
    /// per-session dedup set, and writes fired alerts + privacy-event history to
    /// the shared store.
    pub struct Evaluator {
        store: SharedStore,
        /// (capability_disc, app_id) → active use.
        active: HashMap<(i32, String), ActiveUse>,
        /// (rule_id, capability_disc, app_id) already fired for the current
        /// active session — so a long-running use fires each LONGER_THAN rule once.
        fired_long: HashSet<(i64, i32, String)>,
    }

    /// The proto `CapabilityKind` discriminant for a collector capability.
    fn cap_disc(cap: Capability) -> i32 {
        match cap {
            Capability::Camera => 1,
            Capability::Microphone => 2,
            Capability::Location => 3,
        }
    }

    impl Evaluator {
        pub fn new(store: SharedStore) -> Self {
            Self {
                store,
                active: HashMap::new(),
                fired_long: HashSet::new(),
            }
        }

        /// Runs until `stop` is set or the transition channel closes. Blocks on
        /// the channel with a 1 s timeout; each timeout drives a duration sweep so
        /// `ALERT_LONGER_THAN` fires without needing a registry edge.
        pub fn run(mut self, rx: Receiver<PrivacyTransition>, stop: Arc<AtomicBool>) {
            while !stop.load(Ordering::SeqCst) {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(t) => self.on_transition(t),
                    Err(RecvTimeoutError::Timeout) => self.duration_sweep(),
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        }

        /// Records the transition to history and evaluates discrete conditions,
        /// updating the active-use table.
        fn on_transition(&mut self, t: PrivacyTransition) {
            let cap = cap_disc(t.capability);
            // Persist the raw transition to the privacy_event history (finally
            // populating what the M7 note said stayed empty).
            self.record_event(&t, cap);

            let key = (cap, t.app_id.clone());
            if t.started {
                self.active.insert(
                    key.clone(),
                    ActiveUse {
                        display_name: t.display_name.clone(),
                        app_id: t.app_id.clone(),
                        started_ms: t.ts_ms,
                    },
                );
                // New session → clear any prior long-use dedup for this key.
                self.fired_long
                    .retain(|(_, c, a)| !(*c == cap && *a == t.app_id));

                let unknown_app = app_signature_unknown(&t.app_id, t.packaged);
                let ev = AlertInput {
                    capability: cap,
                    app_id: t.app_id.clone(),
                    display_name: t.display_name.clone(),
                    started: true,
                    foreground: t.foreground,
                    session_locked: t.session_locked,
                    unknown_app,
                    active_seconds: 0,
                    duration_check: false,
                    ts_ms: t.ts_ms,
                };
                self.evaluate_discrete(&ev);
            } else {
                self.active.remove(&key);
                self.fired_long
                    .retain(|(_, c, a)| !(*c == cap && *a == t.app_id));
            }
        }

        /// Evaluates every enabled rule's discrete condition against a start
        /// input, recording a fired alert per match.
        fn evaluate_discrete(&self, ev: &AlertInput) {
            let rules = match self.store.lock() {
                Ok(s) => s.list_enabled_privacy_alert_rules().unwrap_or_default(),
                Err(_) => return,
            };
            for r in rules {
                if condition_matches(r.capability, r.condition, r.threshold_seconds, ev) {
                    let detail = alert_detail(r.condition, r.threshold_seconds, ev);
                    self.record_fired(r.id, ev, &detail);
                }
            }
        }

        /// Periodic pass: for each active use, test `ALERT_LONGER_THAN` rules and
        /// fire once per (rule, session) when the elapsed time crosses threshold.
        fn duration_sweep(&mut self) {
            if self.active.is_empty() {
                return;
            }
            let now = now_ms();
            let rules = match self.store.lock() {
                Ok(s) => s.list_enabled_privacy_alert_rules().unwrap_or_default(),
                Err(_) => return,
            };
            // Snapshot active uses to avoid borrowing self across the mutation.
            let actives: Vec<(i32, String, String, i64)> = self
                .active
                .iter()
                .map(|((c, _), u)| (*c, u.app_id.clone(), u.display_name.clone(), u.started_ms))
                .collect();
            for (cap, app_id, display_name, started_ms) in actives {
                let elapsed = ((now - started_ms).max(0) / 1000) as u32;
                let ev = AlertInput {
                    capability: cap,
                    app_id: app_id.clone(),
                    display_name,
                    started: false,
                    foreground: false,
                    session_locked: false,
                    unknown_app: false,
                    active_seconds: elapsed,
                    duration_check: true,
                    ts_ms: now,
                };
                for r in &rules {
                    if r.condition == super::COND_LONGER_THAN
                        && condition_matches(r.capability, r.condition, r.threshold_seconds, &ev)
                    {
                        let dedup = (r.id, cap, app_id.clone());
                        if self.fired_long.insert(dedup) {
                            let detail = alert_detail(r.condition, r.threshold_seconds, &ev);
                            self.record_fired(r.id, &ev, &detail);
                        }
                    }
                }
            }
        }

        fn record_event(&self, t: &PrivacyTransition, cap: i32) {
            if let Ok(s) = self.store.lock() {
                let _ = s.record_privacy_event(&PrivacyEventRow {
                    ts_ms: t.ts_ms,
                    capability: cap,
                    app_id: t.app_id.clone(),
                    display_name: t.display_name.clone(),
                    started: t.started,
                });
            }
        }

        fn record_fired(&self, rule_id: i64, ev: &AlertInput, detail: &str) {
            if let Ok(s) = self.store.lock() {
                let _ = s.record_fired_alert(&FiredAlertRow {
                    id: 0,
                    rule_id,
                    rule_name: String::new(),
                    ts_ms: ev.ts_ms,
                    capability: ev.capability,
                    app_id: ev.app_id.clone(),
                    display_name: ev.display_name.clone(),
                    detail: detail.to_string(),
                });
                tracing::info!(rule_id, detail, "privacy alert fired");
            }
        }
    }

    /// Whether a desktop app's on-disk image is untrusted (for `ALERT_UNKNOWN_APP`).
    /// Packaged apps are always considered known. Desktop apps are resolved from
    /// the `#`-escaped ConsentStore moniker to a real path and signature-checked.
    fn app_signature_unknown(app_id: &str, packaged: bool) -> bool {
        if packaged {
            return false;
        }
        let path = unmunge_nonpackaged(app_id);
        let signed = verify_signature(&path) == SignatureStatus::Signed;
        is_unknown_app(signed, packaged)
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    #[cfg(test)]
    mod driver_tests {
        use std::sync::{Arc, Mutex};

        use atlas_collectors::{Capability, PrivacyTransition};
        use atlas_store::{PrivacyAlertRuleRow, Store};

        use super::Evaluator;

        fn transition(started: bool, foreground: bool, locked: bool) -> PrivacyTransition {
            PrivacyTransition {
                capability: Capability::Microphone,
                app_id: "C:#app.exe".into(),
                display_name: "app.exe".into(),
                packaged: false,
                started,
                ts_ms: 1_000,
                foreground,
                session_locked: locked,
                active_seconds: 0,
            }
        }

        /// End-to-end evaluator → store: a background-use rule fires on a
        /// background mic start and the fired alert is readable back, with the
        /// transition recorded to privacy_event history. This is the wiring a live
        /// hardware transition would exercise, minus the registry.
        #[test]
        fn background_start_records_fired_alert_and_event() {
            let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
            store
                .lock()
                .unwrap()
                .create_privacy_alert_rule(&PrivacyAlertRuleRow {
                    id: 0,
                    name: "mic background".into(),
                    enabled: true,
                    capability: 2,
                    condition: 2, // ALERT_BACKGROUND_USE
                    threshold_seconds: 0,
                    created_ms: 1,
                })
                .unwrap();

            let mut eval = Evaluator::new(store.clone());
            // Background start (not foreground) → the rule matches.
            eval.on_transition(transition(true, false, false));

            let s = store.lock().unwrap();
            let (alerts, _) = s.list_fired_alerts(0, 10_000, 10).unwrap();
            assert_eq!(alerts.len(), 1, "one fired alert");
            assert_eq!(alerts[0].rule_name, "mic background");
            assert!(alerts[0].detail.contains("microphone"));
            assert!(alerts[0].detail.contains("not in the foreground"));
            // The transition was also written to the privacy_event history.
            let (events, _) = s.list_privacy_events(0, 10_000, 10).unwrap();
            assert_eq!(events.len(), 1);
            assert!(events[0].started);
        }

        /// A foreground start does not satisfy the background-use rule — no alert.
        #[test]
        fn foreground_start_does_not_fire_background_rule() {
            let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
            store
                .lock()
                .unwrap()
                .create_privacy_alert_rule(&PrivacyAlertRuleRow {
                    id: 0,
                    name: "mic background".into(),
                    enabled: true,
                    capability: 2,
                    condition: 2,
                    threshold_seconds: 0,
                    created_ms: 1,
                })
                .unwrap();

            let mut eval = Evaluator::new(store.clone());
            eval.on_transition(transition(true, true, false)); // foreground

            let s = store.lock().unwrap();
            let (alerts, _) = s.list_fired_alerts(0, 10_000, 10).unwrap();
            assert!(
                alerts.is_empty(),
                "foreground use must not fire background rule"
            );
        }
    }
}

#[cfg(windows)]
pub use driver::Evaluator;

#[cfg(test)]
mod tests {
    use super::*;

    fn start_input() -> AlertInput {
        AlertInput {
            capability: 2, // microphone
            app_id: "C:#app.exe".into(),
            display_name: "app.exe".into(),
            started: true,
            foreground: true,
            session_locked: false,
            unknown_app: false,
            active_seconds: 0,
            duration_check: false,
            ts_ms: 1_000,
        }
    }

    #[test]
    fn any_use_fires_on_start_only() {
        let mut ev = start_input();
        assert!(condition_matches(0, COND_ANY_USE, 0, &ev));
        // A stop (started=false) does not fire ANY_USE.
        ev.started = false;
        assert!(!condition_matches(0, COND_ANY_USE, 0, &ev));
    }

    #[test]
    fn capability_filter_scopes_rule() {
        let ev = start_input(); // microphone (2)
        assert!(condition_matches(2, COND_ANY_USE, 0, &ev), "matching cap");
        assert!(condition_matches(0, COND_ANY_USE, 0, &ev), "all-caps");
        assert!(
            !condition_matches(1, COND_ANY_USE, 0, &ev),
            "camera rule skips mic"
        );
    }

    #[test]
    fn background_use_requires_not_foreground() {
        let mut ev = start_input();
        ev.foreground = true;
        assert!(!condition_matches(0, COND_BACKGROUND_USE, 0, &ev));
        ev.foreground = false;
        assert!(condition_matches(0, COND_BACKGROUND_USE, 0, &ev));
    }

    #[test]
    fn while_locked_requires_locked() {
        let mut ev = start_input();
        assert!(!condition_matches(0, COND_WHILE_LOCKED, 0, &ev));
        ev.session_locked = true;
        assert!(condition_matches(0, COND_WHILE_LOCKED, 0, &ev));
    }

    #[test]
    fn unknown_app_requires_unknown() {
        let mut ev = start_input();
        assert!(!condition_matches(0, COND_UNKNOWN_APP, 0, &ev));
        ev.unknown_app = true;
        assert!(condition_matches(0, COND_UNKNOWN_APP, 0, &ev));
    }

    #[test]
    fn longer_than_fires_only_on_duration_check_over_threshold() {
        let mut ev = start_input();
        ev.started = false;
        ev.duration_check = true;
        ev.active_seconds = 45;
        assert!(
            condition_matches(0, COND_LONGER_THAN, 30, &ev),
            "over threshold"
        );
        ev.active_seconds = 10;
        assert!(
            !condition_matches(0, COND_LONGER_THAN, 30, &ev),
            "under threshold"
        );
        // A zero threshold never fires (would be every use — use ANY_USE instead).
        ev.active_seconds = 100;
        assert!(!condition_matches(0, COND_LONGER_THAN, 0, &ev));
        // A discrete start never fires LONGER_THAN.
        let start = start_input();
        assert!(!condition_matches(0, COND_LONGER_THAN, 1, &start));
    }

    #[test]
    fn discrete_conditions_never_fire_on_duration_check() {
        let mut ev = start_input();
        ev.duration_check = true;
        ev.foreground = false;
        ev.session_locked = true;
        ev.unknown_app = true;
        assert!(!condition_matches(0, COND_ANY_USE, 0, &ev));
        assert!(!condition_matches(0, COND_BACKGROUND_USE, 0, &ev));
        assert!(!condition_matches(0, COND_WHILE_LOCKED, 0, &ev));
        assert!(!condition_matches(0, COND_UNKNOWN_APP, 0, &ev));
    }

    #[test]
    fn detail_is_factual_and_never_accusatory() {
        let ev = start_input();
        let d = alert_detail(COND_BACKGROUND_USE, 0, &ev);
        assert!(d.contains("microphone"));
        assert!(d.contains("not in the foreground"));
        // No accusatory vocabulary.
        for bad in ["spyware", "malware", "stealing", "spying", "malicious"] {
            assert!(
                !d.to_lowercase().contains(bad),
                "detail must stay factual: {d}"
            );
        }
    }

    #[test]
    fn unknown_app_only_for_unsigned_desktop() {
        assert!(is_unknown_app(false, false), "unsigned desktop = unknown");
        assert!(!is_unknown_app(true, false), "signed desktop = known");
        assert!(!is_unknown_app(false, true), "packaged always known");
    }
}
