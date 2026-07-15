//! Performance rules engine (docs/phases.md R2, PRD §9.7, tech-stack §4.3).
//!
//! Two halves:
//!
//! * A **pure resolver** ([`resolve`]) — `resolve(rules, process, env) ->
//!   EffectivePolicy` — that is deterministic, conflict-resolving, and shared by
//!   the live applier *and* the dry-run simulation, so a preview can never lie
//!   (it is the same code path). Higher `precedence` wins per action dimension;
//!   ties break by rule id. Protected-critical / session-0 processes resolve to
//!   a `blocked` policy that applies nothing.
//!
//! * A Windows **applier + reversal ledger** ([`RulesEngine`]) that runs on each
//!   sampler tick inside `serve`: it computes each live process's effective
//!   policy, applies only the *deltas* through the collector policy FFI, and
//!   records the ORIGINAL priority/affinity/EcoQoS in an in-memory ledger. When a
//!   rule stops matching (disabled, deleted, power/foreground changed) or the
//!   process is no longer targeted, the original is restored; on shutdown
//!   everything is restored. Every apply and restore appends to the `audit`
//!   table (actor `rules`). Protected-critical processes are never touched.

// The resolver core is pure and platform-independent (unit-tested on the host).
// The applier + ledger use the Windows-only policy FFI.

use atlas_collectors::image_family;

/// When a rule's trigger is active (docs/phases.md R2). Maps 1:1 onto the proto
/// `RuleTrigger` discriminants.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trigger {
    WhileRunning,
    OnAcPower,
    OnDcPower,
    OnFullscreen,
    OnGpuLoad,
    OnGpuThermalThrottle,
}

impl Trigger {
    pub fn from_disc(d: i32) -> Trigger {
        match d {
            2 => Trigger::OnAcPower,
            3 => Trigger::OnDcPower,
            4 => Trigger::OnFullscreen,
            5 => Trigger::OnGpuLoad,
            6 => Trigger::OnGpuThermalThrottle,
            _ => Trigger::WhileRunning,
        }
    }
}

/// A target priority class (proto `PriorityClass`). `Unchanged` leaves it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Priority {
    #[default]
    Unchanged,
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
}

impl Priority {
    pub fn from_disc(d: i32) -> Priority {
        match d {
            1 => Priority::Idle,
            2 => Priority::BelowNormal,
            3 => Priority::Normal,
            4 => Priority::AboveNormal,
            5 => Priority::High,
            _ => Priority::Unchanged,
        }
    }

    /// Human label for simulation output.
    pub fn label(self) -> &'static str {
        match self {
            Priority::Unchanged => "(unchanged)",
            Priority::Idle => "Idle",
            Priority::BelowNormal => "Below Normal",
            Priority::Normal => "Normal",
            Priority::AboveNormal => "Above Normal",
            Priority::High => "High",
        }
    }
}

/// A target core-affinity mode (proto `CoreAffinityMode`). `Unchanged` leaves
/// affinity as-is; `Custom` carries an explicit processor bitmask.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Affinity {
    #[default]
    Unchanged,
    AllCores,
    PreferP,
    PreferE,
    Custom(u64),
}

impl Affinity {
    pub fn from_row(mode: i32, mask: u64) -> Affinity {
        match mode {
            1 => Affinity::AllCores,
            2 => Affinity::PreferP,
            3 => Affinity::PreferE,
            4 => Affinity::Custom(mask),
            _ => Affinity::Unchanged,
        }
    }

    pub fn label(self) -> String {
        match self {
            Affinity::Unchanged => "(unchanged)".to_string(),
            Affinity::AllCores => "All cores".to_string(),
            Affinity::PreferP => "Prefer P-cores".to_string(),
            Affinity::PreferE => "Prefer E-cores".to_string(),
            Affinity::Custom(m) => format!("Custom 0x{m:x}"),
        }
    }
}

/// A rule reduced to the resolver's shape (from a store `RuleRow`). `match_image`
/// is normalised to its lowercase family for case-insensitive matching.
#[derive(Clone, Debug)]
pub struct ResolvableRule {
    pub id: i64,
    pub name: String,
    pub match_family: String,
    pub trigger: Trigger,
    pub priority: Priority,
    pub affinity: Affinity,
    pub eco_qos: bool,
    pub precedence: i32,
    pub gpu_threshold_permille: u32,
    pub gpu_duration_seconds: u32,
}

impl ResolvableRule {
    /// Builds a resolvable rule from a store row (ignores `enabled` — the caller
    /// decides which rules to feed the resolver).
    pub fn from_row(r: &atlas_store::RuleRow) -> ResolvableRule {
        ResolvableRule {
            id: r.id,
            name: r.name.clone(),
            match_family: image_family(&r.match_image),
            trigger: Trigger::from_disc(r.trigger),
            priority: Priority::from_disc(r.priority_class),
            affinity: Affinity::from_row(r.affinity_mode, r.affinity_mask),
            eco_qos: r.eco_qos,
            precedence: r.precedence,
            gpu_threshold_permille: r.gpu_threshold_permille.clamp(10, 1000),
            gpu_duration_seconds: r.gpu_duration_seconds.clamp(1, 300),
        }
    }

    /// Whether this rule matches `image` (case-insensitive family compare). An
    /// empty `match_family` matches nothing (a guard against a blank rule
    /// sweeping every process).
    pub fn matches_image(&self, image: &str) -> bool {
        !self.match_family.is_empty() && self.match_family == image_family(image)
    }

    /// Whether this rule's trigger is active in `env`.
    pub fn trigger_active(&self, env: &Env) -> bool {
        match self.trigger {
            Trigger::WhileRunning => true,
            Trigger::OnAcPower => env.on_ac,
            Trigger::OnDcPower => !env.on_ac,
            Trigger::OnFullscreen => false, // decided per-process by the caller
            Trigger::OnGpuLoad => false, // decided per-process by the caller
            Trigger::OnGpuThermalThrottle => env.gpu_thermal_throttling,
        }
    }
}

/// The environment the resolver evaluates triggers against.
#[derive(Clone, Copy, Debug)]
pub struct Env {
    /// True on AC power, false on battery.
    pub on_ac: bool,
    /// Pid owning the foreground window (the ON_FULLSCREEN trigger).
    pub foreground_pid: u32,
    pub gpu_thermal_throttling: bool,
}

/// One process, as the resolver sees it.
#[derive(Clone, Copy, Debug)]
pub struct ProcInput<'a> {
    pub pid: u32,
    pub image: &'a str,
    /// Precomputed by the caller: protected-critical / session-0 / pid≤4.
    pub protected: bool,
    pub gpu_permille: u32,
}

/// The effective policy for one process after resolving all matching rules.
/// Each action dimension records the rule that won it (for interventions +
/// audit + conflict notes). `blocked` marks a protected-critical target that is
/// never actioned.
#[derive(Clone, Debug, Default)]
pub struct EffectivePolicy {
    pub priority: Priority,
    pub priority_rule: Option<(i64, String)>,
    pub affinity: Affinity,
    pub affinity_rule: Option<(i64, String)>,
    /// `None` = leave EcoQoS; `Some(true)` = a rule requests EcoQoS on.
    pub eco: Option<bool>,
    pub eco_rule: Option<(i64, String)>,
    pub blocked: bool,
    pub blocked_reason: String,
}

impl EffectivePolicy {
    /// Whether the policy asks for any action at all.
    pub fn is_empty(&self) -> bool {
        self.priority == Priority::Unchanged
            && self.affinity == Affinity::Unchanged
            && self.eco.is_none()
    }

    /// A representative winning rule for display (priority, else affinity, else
    /// eco).
    pub fn primary_rule(&self) -> Option<(i64, String)> {
        self.priority_rule
            .clone()
            .or_else(|| self.affinity_rule.clone())
            .or_else(|| self.eco_rule.clone())
    }
}

/// Whether a rule matches a process here and now (image + trigger, including the
/// per-process ON_FULLSCREEN case).
fn rule_applies(rule: &ResolvableRule, proc: &ProcInput, env: &Env) -> bool {
    if !rule.matches_image(proc.image) {
        return false;
    }
    match rule.trigger {
        Trigger::OnFullscreen => env.foreground_pid != 0 && proc.pid == env.foreground_pid,
        Trigger::OnGpuLoad => proc.gpu_permille >= rule.gpu_threshold_permille,
        _ => rule.trigger_active(env),
    }
}

/// The pure resolver (PRD §9.7.6). Deterministic: rules are folded in ascending
/// `(precedence, id)` order so the highest-precedence rule wins each action
/// dimension it specifies; ties break toward the higher id (later-created). A
/// protected process resolves to a `blocked` policy that applies nothing, but
/// still records the rules that *would* have matched (so simulation can show the
/// block).
pub fn resolve(rules: &[ResolvableRule], proc: &ProcInput, env: &Env) -> EffectivePolicy {
    let mut matching: Vec<&ResolvableRule> = rules
        .iter()
        .filter(|r| rule_applies(r, proc, env))
        .collect();
    // Ascending precedence, then ascending id: the last writer (highest
    // precedence, highest id) wins each dimension.
    matching.sort_by(|a, b| a.precedence.cmp(&b.precedence).then(a.id.cmp(&b.id)));

    let mut pol = EffectivePolicy::default();

    if proc.protected {
        pol.blocked = true;
        pol.blocked_reason =
            "protected-critical / session-0 process — the rules engine never touches it"
                .to_string();
        return pol;
    }

    for r in matching {
        if r.priority != Priority::Unchanged {
            pol.priority = r.priority;
            pol.priority_rule = Some((r.id, r.name.clone()));
        }
        if r.affinity != Affinity::Unchanged {
            pol.affinity = r.affinity;
            pol.affinity_rule = Some((r.id, r.name.clone()));
        }
        if r.eco_qos {
            pol.eco = Some(true);
            pol.eco_rule = Some((r.id, r.name.clone()));
        }
    }
    pol
}

/// Whether two triggers can ever be simultaneously active (used for conflict
/// detection): only AC vs DC are mutually exclusive.
fn triggers_can_overlap(a: Trigger, b: Trigger) -> bool {
    !matches!(
        (a, b),
        (Trigger::OnAcPower, Trigger::OnDcPower) | (Trigger::OnDcPower, Trigger::OnAcPower)
    )
}

/// Human-readable conflict notes between `sim` and each other enabled rule
/// (PRD §9.7.6). A conflict exists when both rules can match the same process
/// (same image family, overlapping triggers) and both set the same action
/// dimension. The note states which rule wins by precedence (higher wins; tie →
/// higher id). Pure, so simulation and the UI conflict view agree.
pub fn conflicts_with(sim: &ResolvableRule, others: &[ResolvableRule]) -> Vec<String> {
    let mut notes = Vec::new();
    for o in others {
        if o.id == sim.id {
            continue;
        }
        if sim.match_family.is_empty() || sim.match_family != o.match_family {
            continue;
        }
        if !triggers_can_overlap(sim.trigger, o.trigger) {
            continue;
        }
        // `sim` wins a dimension when it has strictly higher precedence, or equal
        // precedence and a higher id.
        let sim_wins =
            sim.precedence > o.precedence || (sim.precedence == o.precedence && sim.id > o.id);
        let winner = if sim_wins { "this rule" } else { &o.name };
        let dims = [
            (
                sim.priority != Priority::Unchanged && o.priority != Priority::Unchanged,
                "priority",
            ),
            (
                sim.affinity != Affinity::Unchanged && o.affinity != Affinity::Unchanged,
                "affinity",
            ),
            (sim.eco_qos && o.eco_qos, "EcoQoS"),
        ];
        for (clash, dim) in dims {
            if clash {
                notes.push(format!(
                    "{dim} conflicts with rule #{} \"{}\" (precedence {} vs {}) — {} wins",
                    o.id, o.name, sim.precedence, o.precedence, winner
                ));
            }
        }
    }
    notes
}

// ===========================================================================
// Windows applier + reversal ledger.
// ===========================================================================

#[cfg(windows)]
pub use engine::RulesEngine;

#[cfg(windows)]
pub mod engine {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use atlas_collectors::ffi::{
        ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
        IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS,
    };
    use atlas_collectors::{
        cpu_topology, eco_is_on, foreground_pid, get_affinity, get_default_cpu_sets, get_eco_qos,
        get_priority_class, power_is_ac, priority_class_name, restore_eco_qos, set_affinity_mask,
        set_default_cpu_sets, set_eco_qos, set_priority_class, CpuTopology, EcoState, ProcKey,
        SampleSet,
    };
    use atlas_store::{AuditRow, DynProtRow};

    use super::{resolve, Affinity, EffectivePolicy, Env, Priority, ProcInput, ResolvableRule, Trigger};
    use crate::broker::is_protected_process;
    use crate::dynamic_protection::{is_candidate, restore_reason, DynConfig, RestoreReason};
    use crate::ipc::SharedStore;

    /// Converts a persisted [`DynProtRow`] into the live watchdog [`DynConfig`].
    fn dyn_config_from_row(r: DynProtRow) -> DynConfig {
        DynConfig {
            enabled: r.enabled,
            cpu_threshold_permille: r.cpu_threshold_permille,
            sustain_seconds: r.sustain_seconds,
            max_intervention_seconds: r.max_intervention_seconds,
        }
    }

    /// Converts a live watchdog [`DynConfig`] back into a persistable row.
    fn dyn_row_from_config(c: DynConfig) -> DynProtRow {
        DynProtRow {
            enabled: c.enabled,
            cpu_threshold_permille: c.cpu_threshold_permille,
            sustain_seconds: c.sustain_seconds,
            max_intervention_seconds: c.max_intervention_seconds,
        }
    }

    /// Maps a resolver [`Priority`] to its Win32 priority-class value.
    fn priority_win32(p: Priority) -> Option<u32> {
        Some(match p {
            Priority::Unchanged => return None,
            Priority::Idle => IDLE_PRIORITY_CLASS,
            Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
            Priority::Normal => NORMAL_PRIORITY_CLASS,
            Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
            Priority::High => HIGH_PRIORITY_CLASS,
        })
    }

    /// What the applier did to a process's affinity, with the original captured
    /// for reversal. Affinity is applied by exactly one mechanism at a time.
    #[derive(Clone, Debug)]
    enum AffinityApplied {
        /// A hard affinity mask was set; `original` is the pre-intervention mask.
        Mask { original: u64 },
        /// Default CPU sets were assigned (P/E steering); `original` is the
        /// pre-intervention assignment (empty = none / system default).
        CpuSets { original: Vec<u32> },
    }

    /// One live intervention with the ORIGINAL state captured per dimension so it
    /// can be reversed exactly (PRD §3.3). Only touched dimensions carry an
    /// original; a `None` means "we never changed this dimension".
    struct LedgerEntry {
        image_name: String,
        rule_id: i64,
        rule_name: String,
        since_ms: i64,
        orig_priority: Option<u32>,
        affinity: Option<AffinityApplied>,
        orig_eco: Option<EcoState>,
        applied: String,
    }

    impl LedgerEntry {
        fn new(image_name: String, now: i64) -> Self {
            Self {
                image_name,
                rule_id: 0,
                rule_name: String::new(),
                since_ms: now,
                orig_priority: None,
                affinity: None,
                orig_eco: None,
                applied: String::new(),
            }
        }

        fn touches_anything(&self) -> bool {
            self.orig_priority.is_some() || self.affinity.is_some() || self.orig_eco.is_some()
        }
    }

    /// The `rule_name` a dynamic (watchdog) intervention surfaces through the
    /// existing `ListInterventions` (rule_id = 0), per the frozen contract.
    const DYNAMIC_RULE_NAME: &str = "Dynamic responsiveness protection";

    /// One live *dynamic* (watchdog) dampening, with the ORIGINAL state captured
    /// per dimension for exact reversal. Kept in a ledger separate from the
    /// rules ledger so an explicit rule and dynamic protection can never co-own a
    /// dimension of the same process (see [`RulesEngine::apply_tick`]).
    struct DynEntry {
        image_name: String,
        /// When this dampening started (intervention age = now − since_ms).
        since_ms: i64,
        /// Pre-intervention priority class (`None` = was unreadable, so left).
        orig_priority: Option<u32>,
        /// Pre-intervention EcoQoS state, restored verbatim.
        orig_eco: Option<EcoState>,
        /// The factual, human "applied" line surfaced via `ListInterventions`.
        applied: String,
    }

    /// Per-process CPU-streak bookkeeping for the watchdog: when the process most
    /// recently crossed *up* to the threshold, and (once dampened) when it most
    /// recently fell *below* it. Exactly one of the two is `Some` at a time.
    #[derive(Clone, Copy, Default)]
    struct Streak {
        /// First tick (ms) of the current continuous at/above-threshold run.
        above_since: Option<i64>,
        /// First tick (ms) of the current continuous below-threshold run.
        below_since: Option<i64>,
    }

    impl Streak {
        /// Whole seconds continuously at/above threshold (0 if currently below).
        fn above_secs(&self, now: i64) -> i64 {
            self.above_since.map(|t| (now - t) / 1000).unwrap_or(0)
        }
        /// Whole seconds continuously below threshold, or `None` if at/above it
        /// (so the cool-down clock is not running).
        fn below_secs(&self, now: i64) -> Option<i64> {
            self.below_since.map(|t| (now - t) / 1000)
        }
    }

    /// One process's facts for a single applier tick, computed once and reused by
    /// the rules applier and the dynamic watchdog.
    struct Obs {
        key: ProcKey,
        image_name: String,
        cpu_permille: u32,
        protected: bool,
        foreground: bool,
        /// An enabled rule actively governs this process this tick (non-empty,
        /// non-blocked resolved policy). Dynamic protection defers to it.
        governed: bool,
        policy: EffectivePolicy,
    }

    /// A snapshot of one live intervention for `ListInterventions`.
    #[derive(Clone, Debug)]
    pub struct InterventionInfo {
        pub rule_id: i64,
        pub rule_name: String,
        pub pid: u32,
        pub image_name: String,
        pub applied: String,
        pub since_ms: i64,
    }

    /// One simulated target for `SimulateRule` (current vs new, or blocked).
    #[derive(Clone, Debug)]
    pub struct SimTarget {
        pub pid: u32,
        pub image_name: String,
        pub current_priority: String,
        pub new_priority: String,
        pub current_affinity: String,
        pub new_affinity: String,
        pub eco_qos_change: bool,
        pub blocked: bool,
        pub blocked_reason: String,
    }

    /// The result of a dry-run simulation: per-process targets + conflict notes.
    #[derive(Clone, Debug, Default)]
    pub struct SimResult {
        pub targets: Vec<SimTarget>,
        pub conflicts: Vec<String>,
    }

    /// The live rules engine: the reversal ledger + detected CPU topology, over
    /// the shared store (for reading rules and appending audit rows).
    pub struct RulesEngine {
        store: SharedStore,
        ledger: Mutex<HashMap<ProcKey, LedgerEntry>>,
        topo: CpuTopology,
        /// R3 dynamic responsiveness protection (PRD §9.7.3): the live config
        /// (loaded from the store at start, updated live by `SetDynamicProtection`).
        dyn_config: Mutex<DynConfig>,
        /// The watchdog's dampening ledger — separate from `ledger` so a rule and
        /// the watchdog never co-own a process dimension.
        dyn_ledger: Mutex<HashMap<ProcKey, DynEntry>>,
        /// Per-process CPU-streak bookkeeping (threshold hysteresis).
        dyn_streaks: Mutex<HashMap<ProcKey, Streak>>,
        /// First timestamp at/above each explicit GPU-load rule threshold.
        gpu_rule_streaks: Mutex<HashMap<(i64, ProcKey), i64>>,
    }

    impl RulesEngine {
        /// Builds the engine over the shared store, detecting CPU topology once
        /// and loading the persisted dynamic-protection config (disabled by
        /// default, so the watchdog dampens nothing until the user opts in).
        pub fn new(store: SharedStore) -> Self {
            let dyn_config = match store.lock() {
                Ok(s) => s
                    .get_dynamic_protection()
                    .map(dyn_config_from_row)
                    .unwrap_or_default(),
                Err(_) => DynConfig::default(),
            };
            Self {
                store,
                ledger: Mutex::new(HashMap::new()),
                topo: cpu_topology(),
                dyn_config: Mutex::new(dyn_config),
                dyn_ledger: Mutex::new(HashMap::new()),
                dyn_streaks: Mutex::new(HashMap::new()),
                gpu_rule_streaks: Mutex::new(HashMap::new()),
            }
        }

        /// Reads the current trigger environment (AC/DC + foreground pid). An
        /// unknown power state (a desktop with no battery) is treated as AC.
        fn read_env(set: &SampleSet) -> Env {
            Env {
                on_ac: power_is_ac().unwrap_or(true),
                foreground_pid: foreground_pid(),
                gpu_thermal_throttling: set.gpu.adapters.iter().any(|a| a.thermal_throttling == Some(true)),
            }
        }

        /// Loads the enabled rules from the store as resolvable rules.
        fn load_rules(&self) -> Vec<ResolvableRule> {
            match self.store.lock() {
                Ok(store) => store
                    .list_enabled_rules()
                    .unwrap_or_default()
                    .iter()
                    .map(ResolvableRule::from_row)
                    .collect(),
                Err(_) => Vec::new(),
            }
        }

        /// Appends one audit row (actor `rules`). Best-effort — a failed audit
        /// write never blocks the action (mirrors the broker).
        fn audit(&self, action: &str, pid: u32, image: &str, decision: &str, detail: &str) {
            self.audit_as("rules", action, pid, image, decision, detail);
        }

        /// Appends one audit row under an explicit `actor` (the dynamic watchdog
        /// records as `dynamic-protection` so its interventions are auditable and
        /// distinguishable from explicit-rule actions). Best-effort.
        fn audit_as(
            &self,
            actor: &str,
            action: &str,
            pid: u32,
            image: &str,
            decision: &str,
            detail: &str,
        ) {
            let row = AuditRow {
                ts_ms: now_ms(),
                actor: actor.to_string(),
                action: action.to_string(),
                pid,
                image_name: image.to_string(),
                decision: decision.to_string(),
                detail: detail.to_string(),
            };
            if let Ok(store) = self.store.lock() {
                if let Err(e) = store.record_audit(&row) {
                    tracing::warn!("audit write failed: {e}");
                }
            }
        }

        /// The applier tick: resolve every live process's effective policy, apply
        /// the explicit-rule deltas, then run the dynamic-protection watchdog on
        /// the processes no rule governs. Runs on the sampler thread inside
        /// `serve`.
        ///
        /// Ordering matters for reversibility. Policies are resolved *before* any
        /// application. The watchdog's pre-restore pass (handing a process back
        /// when a rule takes it over) runs *before* the rules applier captures
        /// originals, so a rule never records a dynamically-modified value as the
        /// "original" to restore to.
        pub fn apply_tick(&self, set: &SampleSet) {
            let rules = self.load_rules();
            let env = Self::read_env(set);
            let cfg = self.dyn_config_snapshot();
            let now = now_ms();

            let mut ledger = match self.ledger.lock() {
                Ok(l) => l,
                Err(_) => return,
            };

            // 1. Resolve every process's effective policy (pure — applies nothing
            //    yet) and gather the facts both appliers need.
            let mut obs: Vec<Obs> = Vec::with_capacity(set.processes.len());
            let mut gpu_streaks = match self.gpu_rule_streaks.lock() { Ok(v) => v, Err(_) => return };
            for p in &set.processes {
                let key = p.key;
                let protected = is_protected_process(&p.image_name, key.pid, p.session_id);
                let proc = ProcInput {
                    pid: key.pid,
                    image: &p.image_name,
                    protected,
                    gpu_permille: p.gpu_permille,
                };
                let eligible: Vec<_> = rules.iter().filter(|rule| {
                    if rule.trigger != Trigger::OnGpuLoad { return true; }
                    let streak_key = (rule.id, key);
                    if p.gpu_permille < rule.gpu_threshold_permille {
                        gpu_streaks.remove(&streak_key);
                        return false;
                    }
                    let start = *gpu_streaks.entry(streak_key).or_insert(set.ts_ms);
                    set.ts_ms.saturating_sub(start) >= rule.gpu_duration_seconds as i64 * 1000
                }).cloned().collect();
                let policy = resolve(&eligible, &proc, &env);
                let governed = !policy.blocked && !policy.is_empty();
                let foreground = env.foreground_pid != 0 && key.pid == env.foreground_pid;
                obs.push(Obs {
                    key,
                    image_name: p.image_name.clone(),
                    cpu_permille: p.cpu_permille,
                    protected,
                    foreground,
                    governed,
                    policy,
                });
            }

            let live: std::collections::HashSet<ProcKey> = obs.iter().map(|o| o.key).collect();
            gpu_streaks.retain(|(_, key), _| live.contains(key));
            drop(gpu_streaks);

            // 2. Dynamic watchdog: pre-restore (hand governed/expired processes
            //    back to their true originals) BEFORE the rules applier runs, so
            //    rule originals are captured cleanly.
            self.dyn_pre_restore(&obs, &live, &cfg, now);

            // 3. Apply the explicit-rule deltas.
            for o in &obs {
                self.reconcile(&mut ledger, o.key, &o.image_name, &o.policy);
            }

            // 4. Dynamic watchdog: dampen new sustained hogs the rules do not
            //    govern.
            self.dyn_apply(&obs, &cfg, now);

            // 5. Drop ledger entries whose process is gone this tick. We do NOT
            //    try to restore them: the OS already reset the exited process, and
            //    the pid may have been reused (restoring by pid could hit the
            //    wrong process). The (pid, create_time) key guards attribution.
            ledger.retain(|k, e| {
                let keep = live.contains(k);
                if !keep && e.touches_anything() {
                    tracing::debug!(pid = k.pid, "rules: target exited; dropping ledger entry");
                }
                keep
            });
        }

        /// A stable copy of the current dynamic-protection config for a tick.
        fn dyn_config_snapshot(&self) -> DynConfig {
            self.dyn_config.lock().map(|g| *g).unwrap_or_default()
        }

        /// Watchdog pre-restore pass (runs before the rules applier). Restores +
        /// drops any dynamic dampening whose process should no longer be held:
        /// disabled, a rule now governs it, it became foreground, the hard
        /// max-duration cap elapsed, or it exited. Also refreshes CPU streaks so
        /// step 4 can evaluate candidates and cool-downs.
        fn dyn_pre_restore(
            &self,
            obs: &[Obs],
            live: &std::collections::HashSet<ProcKey>,
            cfg: &DynConfig,
            now: i64,
        ) {
            let mut streaks = match self.dyn_streaks.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            // Update the threshold hysteresis for every live process.
            for o in obs {
                let st = streaks.entry(o.key).or_default();
                if o.cpu_permille >= cfg.cpu_threshold_permille {
                    if st.above_since.is_none() {
                        st.above_since = Some(now);
                    }
                    st.below_since = None;
                } else {
                    if st.below_since.is_none() {
                        st.below_since = Some(now);
                    }
                    st.above_since = None;
                }
            }
            // Forget streaks for exited processes (bounded memory).
            streaks.retain(|k, _| live.contains(k));

            let mut dyn_ledger = match self.dyn_ledger.lock() {
                Ok(l) => l,
                Err(_) => return,
            };

            // Index the per-process facts for O(1) lookup.
            let by_key: HashMap<ProcKey, &Obs> = obs.iter().map(|o| (o.key, o)).collect();

            let keys: Vec<ProcKey> = dyn_ledger.keys().copied().collect();
            for key in keys {
                match by_key.get(&key) {
                    None => {
                        // Process exited: drop without restoring (the OS reset it;
                        // the pid may be reused). Same rationale as the rules
                        // ledger retain.
                        if let Some(e) = dyn_ledger.remove(&key) {
                            tracing::debug!(
                                pid = key.pid,
                                image = %e.image_name,
                                "dynamic-protection: target exited; dropping dampening"
                            );
                        }
                    }
                    Some(o) => {
                        let st = streaks.get(&key).copied().unwrap_or_default();
                        let held =
                            (now - dyn_ledger.get(&key).map(|e| e.since_ms).unwrap_or(now)) / 1000;
                        if let Some(reason) =
                            restore_reason(cfg, o.governed, o.foreground, held, st.below_secs(now))
                        {
                            if let Some(entry) = dyn_ledger.remove(&key) {
                                self.dyn_restore(key, entry, reason);
                            }
                        }
                    }
                }
            }
        }

        /// Watchdog apply pass (runs after the rules applier). Dampens sustained
        /// background hogs the rules do not govern — never the foreground app, a
        /// protected/system process, or one already dampened.
        fn dyn_apply(&self, obs: &[Obs], cfg: &DynConfig, now: i64) {
            if !cfg.enabled {
                return;
            }
            let streaks = match self.dyn_streaks.lock() {
                Ok(s) => s,
                Err(_) => return,
            };
            let mut dyn_ledger = match self.dyn_ledger.lock() {
                Ok(l) => l,
                Err(_) => return,
            };
            for o in obs {
                let already_damped = dyn_ledger.contains_key(&o.key);
                let above = streaks.get(&o.key).map(|s| s.above_secs(now)).unwrap_or(0);
                if is_candidate(
                    cfg,
                    o.cpu_permille,
                    above,
                    o.protected,
                    o.foreground,
                    o.governed,
                    already_damped,
                ) {
                    let entry = self.dyn_dampen(o, above, cfg, now);
                    dyn_ledger.insert(o.key, entry);
                }
            }
        }

        /// Dampens one process: EcoQoS (the gentlest reversible lever) plus a
        /// drop to Below Normal priority, capturing the ORIGINAL of each dimension
        /// for exact reversal. Audits under `dynamic-protection` with the trigger
        /// (the WHY) so the intervention can be explained (§9.7.3).
        fn dyn_dampen(&self, o: &Obs, above_secs: i64, cfg: &DynConfig, now: i64) -> DynEntry {
            let pid = o.key.pid;
            let pct = o.cpu_permille / 10;
            let trigger = format!(
                "using {pct}% of total CPU sustained {above_secs}s (threshold {}%, sustain {}s)",
                cfg.cpu_threshold_permille / 10,
                cfg.sustain_seconds
            );

            // EcoQoS first (the gentlest lever): capture original, then enable.
            let orig_eco = Some(get_eco_qos(pid).unwrap_or_default());
            let eco_out = set_eco_qos(pid, Some(true));
            // Then lower priority to Below Normal: capture original, then set.
            let orig_priority = get_priority_class(pid);
            let prio_out = set_priority_class(pid, BELOW_NORMAL_PRIORITY_CLASS);

            self.audit_as(
                "dynamic-protection",
                "APPLY_DYNAMIC",
                pid,
                &o.image_name,
                decision(eco_out.success && prio_out.success),
                &format!(
                    "{trigger}; eco: {}; priority→Below Normal: {}",
                    eco_out.message, prio_out.message
                ),
            );

            DynEntry {
                image_name: o.image_name.clone(),
                since_ms: now,
                orig_priority,
                orig_eco,
                applied: format!(
                    "temporarily set to efficiency mode (Below Normal priority) — \
                     using {pct}% CPU in the background"
                ),
            }
        }

        /// Restores one dynamic dampening to its captured original and audits the
        /// reversal with its reason (§9.7.3 "explain why").
        fn dyn_restore(&self, key: ProcKey, entry: DynEntry, reason: RestoreReason) {
            let pid = key.pid;
            if let Some(orig) = entry.orig_eco {
                let out = restore_eco_qos(pid, orig);
                self.audit_as(
                    "dynamic-protection",
                    "RESTORE_DYNAMIC",
                    pid,
                    &entry.image_name,
                    decision(out.success),
                    &format!("{} — eco: {}", reason.label(), out.message),
                );
            }
            if let Some(orig) = entry.orig_priority {
                let out = set_priority_class(pid, orig);
                self.audit_as(
                    "dynamic-protection",
                    "RESTORE_DYNAMIC",
                    pid,
                    &entry.image_name,
                    decision(out.success),
                    &format!(
                        "{} — priority→{}: {}",
                        reason.label(),
                        priority_class_name(orig),
                        out.message
                    ),
                );
            }
        }

        /// Restores every live *dynamic* dampening immediately and clears the
        /// watchdog ledger (called when dynamic protection is disabled — the
        /// change takes effect at once, not on the next tick).
        pub fn restore_all_dynamic(&self, reason: RestoreReason) {
            let entries: Vec<(ProcKey, DynEntry)> = match self.dyn_ledger.lock() {
                Ok(mut l) => l.drain().collect(),
                Err(p) => p.into_inner().drain().collect(),
            };
            let n = entries.len();
            for (key, entry) in entries {
                self.dyn_restore(key, entry, reason);
            }
            if n > 0 {
                tracing::info!(restored = n, "dynamic-protection restored all dampenings");
            }
        }

        /// The current dynamic-protection config (for `GetDynamicProtection`).
        pub fn dynamic_config(&self) -> DynConfig {
            self.dyn_config_snapshot()
        }

        /// Applies a new dynamic-protection config live (for
        /// `SetDynamicProtection`): persists it, swaps the in-memory config, and —
        /// if it disables protection — restores every active dampening at once.
        /// Enabling simply lets the next sampler tick begin evaluating candidates.
        pub fn set_dynamic_config(&self, cfg: DynConfig) {
            if let Ok(store) = self.store.lock() {
                if let Err(e) = store.set_dynamic_protection(&dyn_row_from_config(cfg)) {
                    tracing::warn!("persisting dynamic-protection config failed: {e}");
                }
            }
            if let Ok(mut g) = self.dyn_config.lock() {
                *g = cfg;
            }
            if !cfg.enabled {
                self.restore_all_dynamic(RestoreReason::Disabled);
            }
        }

        /// Reconciles one process against its resolved policy, applying only the
        /// deltas and capturing originals on first touch.
        fn reconcile(
            &self,
            ledger: &mut HashMap<ProcKey, LedgerEntry>,
            key: ProcKey,
            image: &str,
            policy: &super::EffectivePolicy,
        ) {
            // A blocked (protected) process is never touched; if somehow it has a
            // ledger entry (it never should), leave it — restoration by pid on a
            // protected process is itself unsafe.
            if policy.blocked {
                return;
            }

            let has_entry = ledger.contains_key(&key);
            if policy.is_empty() && !has_entry {
                return; // nothing to do and nothing applied
            }

            let now = now_ms();
            let entry = ledger
                .entry(key)
                .or_insert_with(|| LedgerEntry::new(image.to_string(), now));

            // Record the representative winning rule for display.
            if let Some((rid, rname)) = policy.primary_rule() {
                entry.rule_id = rid;
                entry.rule_name = rname;
            }

            self.reconcile_priority(&key, entry, policy.priority);
            self.reconcile_affinity(&key, entry, policy.affinity);
            self.reconcile_eco(&key, entry, policy.eco);

            entry.applied = summarize(entry);

            // If nothing is applied anymore, drop the entry so it stops showing
            // as an intervention.
            if !entry.touches_anything() {
                ledger.remove(&key);
            }
        }

        fn reconcile_priority(&self, key: &ProcKey, entry: &mut LedgerEntry, want: Priority) {
            let pid = key.pid;
            match priority_win32(want) {
                Some(target) => {
                    let cur = get_priority_class(pid);
                    if entry.orig_priority.is_none() {
                        entry.orig_priority = cur; // capture pre-intervention value
                    }
                    if cur != Some(target) {
                        let out = set_priority_class(pid, target);
                        self.audit(
                            "APPLY_PRIORITY",
                            pid,
                            &entry.image_name,
                            decision(out.success),
                            &out.message,
                        );
                    }
                }
                None => {
                    if let Some(orig) = entry.orig_priority.take() {
                        let out = set_priority_class(pid, orig);
                        self.audit(
                            "RESTORE_PRIORITY",
                            pid,
                            &entry.image_name,
                            decision(out.success),
                            &format!("restore {} — {}", priority_class_name(orig), out.message),
                        );
                    }
                }
            }
        }

        fn reconcile_affinity(&self, key: &ProcKey, entry: &mut LedgerEntry, want: Affinity) {
            let pid = key.pid;
            // Compute the desired mechanism from the mode + topology. P/E steering
            // degrades to a no-op on a homogeneous machine (honestly skipped).
            let desired = self.desired_affinity(pid, want);

            match (&desired, entry.affinity.take()) {
                // Nothing wanted, nothing applied: no-op.
                (None, None) => {}
                // Nothing wanted, something applied: restore + clear.
                (None, Some(applied)) => {
                    self.restore_affinity(pid, &entry.image_name, applied);
                }
                // Wanted, nothing applied: capture original + apply.
                (Some(d), None) => {
                    entry.affinity = Some(self.apply_affinity_capturing(pid, &entry.image_name, d));
                }
                // Wanted, already applied: keep the original. If the mechanism
                // kind changed, restore the old first, then apply the new; if it
                // is the same kind, re-apply only on drift (no per-tick spam).
                (Some(d), Some(applied)) => {
                    if mechanism_matches(d, &applied) {
                        if self.affinity_drifted(pid, d) {
                            self.raw_apply_affinity(pid, &entry.image_name, d);
                        }
                        entry.affinity = Some(applied); // original preserved
                    } else {
                        self.restore_affinity(pid, &entry.image_name, applied);
                        entry.affinity =
                            Some(self.apply_affinity_capturing(pid, &entry.image_name, d));
                    }
                }
            }
        }

        /// Whether the process's current affinity differs from the desired one
        /// (so a re-apply is warranted). A read failure reports "drifted" so we
        /// re-assert intent.
        fn affinity_drifted(&self, pid: u32, d: &DesiredAff) -> bool {
            match d {
                DesiredAff::Mask(target) => {
                    get_affinity(pid).map(|a| a.process_mask) != Some(*target)
                }
                DesiredAff::CpuSets(ids) => {
                    get_default_cpu_sets(pid).map(|v| v != *ids).unwrap_or(true)
                }
            }
        }

        /// Resolves an [`Affinity`] mode into a concrete desired mechanism using
        /// the current process/system masks + detected topology. Returns `None`
        /// when the mode is `Unchanged` or a P/E request that this machine cannot
        /// satisfy (homogeneous cores).
        fn desired_affinity(&self, pid: u32, want: Affinity) -> Option<DesiredAff> {
            match want {
                Affinity::Unchanged => None,
                Affinity::AllCores => {
                    let sys = get_affinity(pid)?.system_mask;
                    (sys != 0).then_some(DesiredAff::Mask(sys))
                }
                Affinity::Custom(mask) => {
                    let sys = get_affinity(pid)?.system_mask;
                    let m = mask & sys;
                    (m != 0).then_some(DesiredAff::Mask(m))
                }
                Affinity::PreferP => {
                    if self.topo.heterogeneous && !self.topo.p_core_ids.is_empty() {
                        Some(DesiredAff::CpuSets(self.topo.p_core_ids.clone()))
                    } else {
                        None
                    }
                }
                Affinity::PreferE => {
                    if self.topo.heterogeneous && !self.topo.e_core_ids.is_empty() {
                        Some(DesiredAff::CpuSets(self.topo.e_core_ids.clone()))
                    } else {
                        None
                    }
                }
            }
        }

        /// Applies a desired affinity, capturing the pre-intervention original
        /// inside the returned [`AffinityApplied`] for reversal.
        fn apply_affinity_capturing(
            &self,
            pid: u32,
            image: &str,
            d: &DesiredAff,
        ) -> AffinityApplied {
            match d {
                DesiredAff::Mask(_) => {
                    let original = get_affinity(pid).map(|a| a.process_mask).unwrap_or(0);
                    self.raw_apply_affinity(pid, image, d);
                    AffinityApplied::Mask { original }
                }
                DesiredAff::CpuSets(_) => {
                    let original = get_default_cpu_sets(pid).unwrap_or_default();
                    self.raw_apply_affinity(pid, image, d);
                    AffinityApplied::CpuSets { original }
                }
            }
        }

        /// Applies a desired affinity mechanism and audits, without touching the
        /// captured original (drift re-assert / initial apply).
        fn raw_apply_affinity(&self, pid: u32, image: &str, d: &DesiredAff) {
            let out = match d {
                DesiredAff::Mask(target) => set_affinity_mask(pid, *target),
                DesiredAff::CpuSets(ids) => set_default_cpu_sets(pid, ids),
            };
            self.audit(
                "APPLY_AFFINITY",
                pid,
                image,
                decision(out.success),
                &out.message,
            );
        }

        /// Restores a process's affinity to the captured original.
        fn restore_affinity(&self, pid: u32, image: &str, applied: AffinityApplied) {
            let out = match applied {
                AffinityApplied::Mask { original } if original != 0 => {
                    set_affinity_mask(pid, original)
                }
                AffinityApplied::Mask { .. } => {
                    // Original unknown (0): fall back to the full system mask.
                    match get_affinity(pid).map(|a| a.system_mask) {
                        Some(sys) if sys != 0 => set_affinity_mask(pid, sys),
                        _ => return,
                    }
                }
                AffinityApplied::CpuSets { original } => set_default_cpu_sets(pid, &original),
            };
            self.audit(
                "RESTORE_AFFINITY",
                pid,
                image,
                decision(out.success),
                &out.message,
            );
        }

        fn reconcile_eco(&self, key: &ProcKey, entry: &mut LedgerEntry, want: Option<bool>) {
            let pid = key.pid;
            match want {
                Some(true) => {
                    if entry.orig_eco.is_none() {
                        // Capture the original; an unreadable state is treated as
                        // system-managed (all-zero) so restore resets cleanly.
                        entry.orig_eco = Some(get_eco_qos(pid).unwrap_or_default());
                    }
                    // Re-enable only if not already throttled (drift correction).
                    let already = get_eco_qos(pid).map(|s| eco_is_on(&s)).unwrap_or(false);
                    if !already {
                        let out = set_eco_qos(pid, Some(true));
                        self.audit(
                            "APPLY_ECO",
                            pid,
                            &entry.image_name,
                            decision(out.success),
                            &out.message,
                        );
                    }
                }
                _ => {
                    if let Some(orig) = entry.orig_eco.take() {
                        let out = restore_eco_qos(pid, orig);
                        self.audit(
                            "RESTORE_ECO",
                            pid,
                            &entry.image_name,
                            decision(out.success),
                            &out.message,
                        );
                    }
                }
            }
        }

        /// Snapshot of all live interventions for `ListInterventions` — both
        /// explicit-rule interventions and dynamic (watchdog) dampenings. Dynamic
        /// dampenings surface with `rule_id = 0` and the reserved rule name, per
        /// the frozen contract.
        pub fn interventions(&self) -> Vec<InterventionInfo> {
            let mut out = Vec::new();
            if let Ok(ledger) = self.ledger.lock() {
                out.extend(
                    ledger
                        .iter()
                        .filter(|(_, e)| e.touches_anything())
                        .map(|(k, e)| InterventionInfo {
                            rule_id: e.rule_id,
                            rule_name: e.rule_name.clone(),
                            pid: k.pid,
                            image_name: e.image_name.clone(),
                            applied: e.applied.clone(),
                            since_ms: e.since_ms,
                        }),
                );
            }
            if let Ok(dyn_ledger) = self.dyn_ledger.lock() {
                out.extend(dyn_ledger.iter().map(|(k, e)| InterventionInfo {
                    rule_id: 0,
                    rule_name: DYNAMIC_RULE_NAME.to_string(),
                    pid: k.pid,
                    image_name: e.image_name.clone(),
                    applied: e.applied.clone(),
                    since_ms: e.since_ms,
                }));
            }
            out
        }

        /// Restores every live intervention and clears the ledger. Called when
        /// the sampler thread stops (service shutdown / Ctrl+C / duration).
        pub fn restore_all(&self) {
            let mut ledger = match self.ledger.lock() {
                Ok(l) => l,
                Err(p) => p.into_inner(),
            };
            let entries: Vec<(ProcKey, LedgerEntry)> = ledger.drain().collect();
            let mut restored = 0u32;
            for (key, mut entry) in entries {
                let pid = key.pid;
                if let Some(orig) = entry.orig_priority.take() {
                    let out = set_priority_class(pid, orig);
                    self.audit(
                        "RESTORE_PRIORITY",
                        pid,
                        &entry.image_name,
                        decision(out.success),
                        &format!("shutdown restore — {}", out.message),
                    );
                    restored += 1;
                }
                if let Some(applied) = entry.affinity.take() {
                    self.restore_affinity(pid, &entry.image_name, applied);
                    restored += 1;
                }
                if let Some(orig) = entry.orig_eco.take() {
                    let out = restore_eco_qos(pid, orig);
                    self.audit(
                        "RESTORE_ECO",
                        pid,
                        &entry.image_name,
                        decision(out.success),
                        &format!("shutdown restore — {}", out.message),
                    );
                    restored += 1;
                }
            }
            if restored > 0 {
                tracing::info!(restored, "rules engine restored interventions on shutdown");
            }
            // Reversibility also covers the dynamic watchdog: restore every live
            // dampening so nothing is left in efficiency mode after shutdown.
            self.restore_all_dynamic(RestoreReason::Shutdown);
        }

        /// Pure dry-run simulation of `sim` against the current live snapshot
        /// (PRD §9.7.5). Applies nothing: for each matching process it reports
        /// current vs new priority/affinity + whether EcoQoS would change, and
        /// marks protected-critical targets `blocked`. `others` are the other
        /// enabled rules, for conflict notes. This uses the same [`resolve`] the
        /// applier does, so the preview cannot diverge from reality.
        pub fn simulate(&self, sim: &ResolvableRule, others: &[ResolvableRule]) -> SimResult {
            let mut gpu_collector = atlas_collectors::GpuCollector::new();
            let gpu = gpu_collector.sample();
            let gpu_by_pid: HashMap<u32, u32> = gpu.processes.iter()
                .map(|p| (p.pid, p.utilization_permille)).collect();
            let env = Env {
                on_ac: power_is_ac().unwrap_or(true),
                foreground_pid: foreground_pid(),
                gpu_thermal_throttling: gpu.adapters.iter().any(|a| a.thermal_throttling == Some(true)),
            };
            // Resolve using only the simulated rule so the "new" column reflects
            // exactly what this rule would do (conflicts are reported separately).
            let one = [sim.clone()];
            let procs = atlas_collectors::snapshot_processes().unwrap_or_default();
            let mut targets = Vec::new();
            for p in &procs {
                if p.pid == 0 {
                    continue;
                }
                let protected = is_protected_process(&p.image_name, p.pid, p.session_id);
                let proc = ProcInput {
                    pid: p.pid,
                    image: &p.image_name,
                    protected,
                    gpu_permille: gpu_by_pid.get(&p.pid).copied().unwrap_or(0),
                };
                // Only surface processes this rule actually matches.
                if !sim.matches_image(&p.image_name) {
                    continue;
                }
                let pol = resolve(&one, &proc, &env);
                // A matched-but-trigger-inactive rule yields an empty, non-blocked
                // policy — skip those (they wouldn't do anything right now).
                if !pol.blocked && pol.is_empty() {
                    continue;
                }

                if pol.blocked {
                    targets.push(SimTarget {
                        pid: p.pid,
                        image_name: p.image_name.clone(),
                        current_priority: String::new(),
                        new_priority: String::new(),
                        current_affinity: String::new(),
                        new_affinity: String::new(),
                        eco_qos_change: false,
                        blocked: true,
                        blocked_reason: pol.blocked_reason.clone(),
                    });
                    continue;
                }

                let cur_prio = get_priority_class(p.pid)
                    .map(priority_class_name)
                    .unwrap_or("Unknown")
                    .to_string();
                let new_prio = if pol.priority == Priority::Unchanged {
                    cur_prio.clone()
                } else {
                    pol.priority.label().to_string()
                };
                let cur_aff = get_affinity(p.pid)
                    .map(|a| format!("0x{:x}", a.process_mask))
                    .unwrap_or_else(|| "?".to_string());
                let new_aff = if pol.affinity == Affinity::Unchanged {
                    cur_aff.clone()
                } else {
                    pol.affinity.label()
                };
                let eco_change = matches!(pol.eco, Some(true))
                    && !get_eco_qos(p.pid).map(|s| eco_is_on(&s)).unwrap_or(false);

                targets.push(SimTarget {
                    pid: p.pid,
                    image_name: p.image_name.clone(),
                    current_priority: cur_prio,
                    new_priority: new_prio,
                    current_affinity: cur_aff,
                    new_affinity: new_aff,
                    eco_qos_change: eco_change,
                    blocked: false,
                    blocked_reason: String::new(),
                });
            }
            SimResult {
                targets,
                conflicts: super::conflicts_with(sim, others),
            }
        }
    }

    /// A concrete affinity mechanism the applier will use this tick.
    enum DesiredAff {
        Mask(u64),
        CpuSets(Vec<u32>),
    }

    /// Whether a desired mechanism is the same *kind* as one already applied.
    fn mechanism_matches(d: &DesiredAff, applied: &AffinityApplied) -> bool {
        matches!(
            (d, applied),
            (DesiredAff::Mask(_), AffinityApplied::Mask { .. })
                | (DesiredAff::CpuSets(_), AffinityApplied::CpuSets { .. })
        )
    }

    /// Builds the human "applied" summary for an intervention from its ledger
    /// entry (only the dimensions currently in effect).
    fn summarize(entry: &LedgerEntry) -> String {
        let mut parts = Vec::new();
        if entry.orig_priority.is_some() {
            parts.push("priority".to_string());
        }
        if let Some(a) = &entry.affinity {
            parts.push(match a {
                AffinityApplied::Mask { .. } => "affinity".to_string(),
                AffinityApplied::CpuSets { .. } => "P/E steering".to_string(),
            });
        }
        if entry.orig_eco.is_some() {
            parts.push("EcoQoS".to_string());
        }
        if parts.is_empty() {
            "(none)".to_string()
        } else {
            parts.join(" + ")
        }
    }

    /// Audit decision tag from a success flag.
    fn decision(ok: bool) -> &'static str {
        if ok {
            "OK"
        } else {
            "FAIL"
        }
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(
        id: i64,
        image: &str,
        trigger: Trigger,
        priority: Priority,
        affinity: Affinity,
        eco: bool,
        precedence: i32,
    ) -> ResolvableRule {
        ResolvableRule {
            id,
            name: format!("rule{id}"),
            match_family: image_family(image),
            trigger,
            priority,
            affinity,
            eco_qos: eco,
            precedence,
            gpu_threshold_permille: 800,
            gpu_duration_seconds: 5,
        }
    }

    fn proc(pid: u32, image: &str) -> ProcInput<'_> {
        ProcInput {
            pid,
            image,
            protected: false,
            gpu_permille: 0,
        }
    }

    const AC: Env = Env {
        on_ac: true,
        foreground_pid: 0,
        gpu_thermal_throttling: false,
    };

    #[test]
    fn no_match_yields_empty_policy() {
        let rules = [rule(
            1,
            "chrome.exe",
            Trigger::WhileRunning,
            Priority::BelowNormal,
            Affinity::Unchanged,
            true,
            0,
        )];
        let pol = resolve(&rules, &proc(100, "notepad.exe"), &AC);
        assert!(pol.is_empty());
        assert!(pol.priority_rule.is_none());
    }

    #[test]
    fn single_rule_applies() {
        let rules = [rule(
            1,
            "chrome.exe",
            Trigger::WhileRunning,
            Priority::BelowNormal,
            Affinity::Unchanged,
            true,
            0,
        )];
        let pol = resolve(&rules, &proc(100, "chrome.exe"), &AC);
        assert_eq!(pol.priority, Priority::BelowNormal);
        assert_eq!(pol.eco, Some(true));
        assert_eq!(pol.priority_rule.as_ref().unwrap().0, 1);
    }

    #[test]
    fn higher_precedence_wins_conflict() {
        let low = rule(
            1,
            "game.exe",
            Trigger::WhileRunning,
            Priority::BelowNormal,
            Affinity::Unchanged,
            false,
            5,
        );
        let high = rule(
            2,
            "game.exe",
            Trigger::WhileRunning,
            Priority::High,
            Affinity::Unchanged,
            false,
            10,
        );
        // Order in the slice must not matter — resolve sorts by precedence.
        let pol = resolve(&[low.clone(), high.clone()], &proc(1, "game.exe"), &AC);
        assert_eq!(pol.priority, Priority::High);
        assert_eq!(pol.priority_rule.as_ref().unwrap().0, 2);
        let pol2 = resolve(&[high, low], &proc(1, "game.exe"), &AC);
        assert_eq!(pol2.priority, Priority::High);
    }

    #[test]
    fn equal_precedence_breaks_toward_higher_id() {
        let a = rule(
            1,
            "x.exe",
            Trigger::WhileRunning,
            Priority::Idle,
            Affinity::Unchanged,
            false,
            5,
        );
        let b = rule(
            2,
            "x.exe",
            Trigger::WhileRunning,
            Priority::High,
            Affinity::Unchanged,
            false,
            5,
        );
        let pol = resolve(&[a, b], &proc(1, "x.exe"), &AC);
        assert_eq!(pol.priority, Priority::High, "higher id wins the tie");
    }

    #[test]
    fn dimensions_resolved_independently() {
        // One rule sets priority, another (higher precedence) sets affinity.
        let prio = rule(
            1,
            "app.exe",
            Trigger::WhileRunning,
            Priority::BelowNormal,
            Affinity::Unchanged,
            false,
            1,
        );
        let aff = rule(
            2,
            "app.exe",
            Trigger::WhileRunning,
            Priority::Unchanged,
            Affinity::AllCores,
            false,
            9,
        );
        let pol = resolve(&[prio, aff], &proc(1, "app.exe"), &AC);
        assert_eq!(pol.priority, Priority::BelowNormal);
        assert_eq!(pol.affinity, Affinity::AllCores);
        assert_eq!(pol.priority_rule.unwrap().0, 1);
        assert_eq!(pol.affinity_rule.unwrap().0, 2);
    }

    #[test]
    fn ac_dc_trigger_gating() {
        let ac_rule = rule(
            1,
            "app.exe",
            Trigger::OnAcPower,
            Priority::High,
            Affinity::Unchanged,
            false,
            0,
        );
        let dc_rule = rule(
            2,
            "app.exe",
            Trigger::OnDcPower,
            Priority::Idle,
            Affinity::Unchanged,
            false,
            0,
        );
        let on_ac = Env {
            on_ac: true,
            foreground_pid: 0,
            gpu_thermal_throttling: false,
        };
        let on_dc = Env {
            on_ac: false,
            foreground_pid: 0,
            gpu_thermal_throttling: false,
        };
        let p1 = resolve(
            &[ac_rule.clone(), dc_rule.clone()],
            &proc(1, "app.exe"),
            &on_ac,
        );
        assert_eq!(p1.priority, Priority::High, "AC rule active on AC");
        let p2 = resolve(&[ac_rule, dc_rule], &proc(1, "app.exe"), &on_dc);
        assert_eq!(p2.priority, Priority::Idle, "DC rule active on battery");
    }

    #[test]
    fn fullscreen_trigger_gates_on_foreground_pid() {
        let fs = rule(
            1,
            "game.exe",
            Trigger::OnFullscreen,
            Priority::High,
            Affinity::Unchanged,
            false,
            0,
        );
        let env_fg = Env {
            on_ac: true,
            foreground_pid: 500,
            gpu_thermal_throttling: false,
        };
        // The foreground process matches → active.
        let fg = resolve(std::slice::from_ref(&fs), &proc(500, "game.exe"), &env_fg);
        assert_eq!(fg.priority, Priority::High);
        // A background instance of the same image → inactive.
        let bg = resolve(&[fs], &proc(501, "game.exe"), &env_fg);
        assert!(bg.is_empty());
    }

    #[test]
    fn protected_process_is_blocked_and_untouched() {
        let rules = [rule(
            1,
            "lsass.exe",
            Trigger::WhileRunning,
            Priority::Idle,
            Affinity::AllCores,
            true,
            0,
        )];
        let mut p = proc(700, "lsass.exe");
        p.protected = true;
        let pol = resolve(&rules, &p, &AC);
        assert!(pol.blocked);
        assert!(pol.is_empty(), "blocked policy applies nothing");
        assert!(!pol.blocked_reason.is_empty());
    }

    #[test]
    fn empty_match_image_matches_nothing() {
        let rules = [rule(
            1,
            "",
            Trigger::WhileRunning,
            Priority::High,
            Affinity::Unchanged,
            false,
            0,
        )];
        assert!(resolve(&rules, &proc(1, "anything.exe"), &AC).is_empty());
    }

    #[test]
    fn conflict_notes_name_the_winner() {
        let sim = rule(
            1,
            "game.exe",
            Trigger::WhileRunning,
            Priority::High,
            Affinity::Unchanged,
            false,
            10,
        );
        let other = rule(
            2,
            "game.exe",
            Trigger::WhileRunning,
            Priority::Idle,
            Affinity::Unchanged,
            false,
            5,
        );
        let notes = conflicts_with(&sim, &[other]);
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("priority"));
        assert!(notes[0].contains("this rule wins"), "{}", notes[0]);

        // Non-overlapping triggers (AC vs DC) do not conflict.
        let ac = rule(
            1,
            "game.exe",
            Trigger::OnAcPower,
            Priority::High,
            Affinity::Unchanged,
            false,
            10,
        );
        let dc = rule(
            2,
            "game.exe",
            Trigger::OnDcPower,
            Priority::Idle,
            Affinity::Unchanged,
            false,
            5,
        );
        assert!(conflicts_with(&ac, &[dc]).is_empty());
    }
}
