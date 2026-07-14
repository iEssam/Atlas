//! Dynamic responsiveness protection — the pure decision core (PRD §9.7.3,
//! docs/phases.md Phase 3 / R3, tech-stack §4.3).
//!
//! A safety-critical watchdog that, only when explicitly enabled, TEMPORARILY
//! dampens a background process monopolizing the CPU and auto-restores it. This
//! module holds the *pure, platform-independent* decision logic so it can be
//! exhaustively unit-tested on the host with no Windows FFI:
//!
//! * [`is_candidate`] — should a process be dampened this tick?
//! * [`restore_reason`] — should an active dampening be reversed, and why?
//!
//! The Windows applier that turns these decisions into EcoQoS / priority changes
//! (capturing the original state for exact reversal) lives in
//! [`crate::rules`] alongside the rules-engine reversal ledger it shares, so an
//! explicit rule and dynamic protection can never fight over the same process.
//!
//! # Safety invariants (NON-NEGOTIABLE — this modifies a running system)
//! A process is **never** a candidate when it is:
//!   * protected-critical / session-0 / system (`is_protected_process`), OR
//!   * the current foreground process (never hurt what the user is using), OR
//!   * already governed by an explicit rule (rules win; §precedence), OR
//!   * already dampened (no double-dampen).
//! Every dampening is bounded by `max_intervention_seconds` — a hard cap that
//! guarantees no process is ever held dampened indefinitely.

/// The live watchdog configuration (mirrors the store `DynProtRow` and the proto
/// `DynamicProtectionConfig`). Copied into the applier so a tick reads a stable
/// snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DynConfig {
    /// Off by default — the watchdog dampens nothing until the user opts in.
    pub enabled: bool,
    /// A process must reach this system-CPU share (permille, 0..=1000) to be a
    /// dampening candidate.
    pub cpu_threshold_permille: u32,
    /// ...sustained continuously for at least this long before any intervention.
    pub sustain_seconds: u32,
    /// Hard auto-restore cap: never hold a dampening longer than this.
    pub max_intervention_seconds: u32,
}

impl Default for DynConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cpu_threshold_permille: 800,
            sustain_seconds: 30,
            max_intervention_seconds: 300,
        }
    }
}

impl DynConfig {
    /// The cool-down a dampened process must stay below threshold before it is
    /// restored on the "calmed down" trigger. Mirrors `sustain_seconds` so the
    /// hysteresis is symmetric (as hard to leave dampening as to enter it).
    pub fn cooldown_seconds(&self) -> i64 {
        self.sustain_seconds as i64
    }
}

/// Why an active dynamic dampening should be reversed. Recorded in the audit
/// trail and folded into the intervention's factual explanation (§9.7.3 "explain
/// why an intervention occurred").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreReason {
    /// Dynamic protection was turned off.
    Disabled,
    /// An explicit rule now governs this process (rules win over the watchdog).
    RuleGoverns,
    /// The process became the foreground app — never keep the user's active app
    /// dampened.
    BecameForeground,
    /// The hard `max_intervention_seconds` cap elapsed.
    MaxDuration,
    /// The process's CPU fell back below threshold for the cool-down window.
    Calmed,
    /// The service is shutting down — restore everything (reversibility, §3.3).
    Shutdown,
}

impl RestoreReason {
    /// A short, factual phrase for the audit log / intervention record.
    pub fn label(self) -> &'static str {
        match self {
            RestoreReason::Disabled => "dynamic protection disabled",
            RestoreReason::RuleGoverns => "an explicit rule now governs this process",
            RestoreReason::BecameForeground => "became the foreground app",
            RestoreReason::MaxDuration => "maximum intervention time reached",
            RestoreReason::Calmed => "CPU returned to normal",
            RestoreReason::Shutdown => "service shutdown",
        }
    }
}

/// Whether a process should be dampened *this tick*, given how long it has been
/// continuously at/above threshold (`above_secs`). Pure and total — the single
/// source of truth for candidate selection, exercised directly by unit tests.
///
/// Returns false unless dynamic protection is enabled AND the process is a
/// sustained hog AND none of the safety exclusions apply AND it is not already
/// dampened.
#[allow(clippy::too_many_arguments)]
pub fn is_candidate(
    cfg: &DynConfig,
    cpu_permille: u32,
    above_secs: i64,
    protected: bool,
    foreground: bool,
    rule_governed: bool,
    already_damped: bool,
) -> bool {
    cfg.enabled
        && !protected
        && !foreground
        && !rule_governed
        && !already_damped
        && cpu_permille >= cfg.cpu_threshold_permille
        && above_secs >= cfg.sustain_seconds as i64
}

/// Whether an *active* dynamic dampening should be reversed, and why (`None` =
/// keep dampening). Pure and total.
///
/// * `held_secs` — how long the dampening has been in effect.
/// * `below_secs` — how long the process has been continuously below threshold
///   (`None` if it is currently at/above threshold, so the cool-down clock is
///   not running).
///
/// Precedence of triggers (most decisive first): disabled → rule takeover →
/// became foreground → hard max-duration cap → calmed down.
pub fn restore_reason(
    cfg: &DynConfig,
    rule_governed: bool,
    foreground: bool,
    held_secs: i64,
    below_secs: Option<i64>,
) -> Option<RestoreReason> {
    if !cfg.enabled {
        return Some(RestoreReason::Disabled);
    }
    if rule_governed {
        return Some(RestoreReason::RuleGoverns);
    }
    if foreground {
        return Some(RestoreReason::BecameForeground);
    }
    if held_secs >= cfg.max_intervention_seconds as i64 {
        return Some(RestoreReason::MaxDuration);
    }
    if let Some(below) = below_secs {
        if below >= cfg.cooldown_seconds() {
            return Some(RestoreReason::Calmed);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DynConfig {
        DynConfig {
            enabled: true,
            cpu_threshold_permille: 800,
            sustain_seconds: 30,
            max_intervention_seconds: 300,
        }
    }

    #[test]
    fn candidate_requires_threshold_and_sustain() {
        let c = cfg();
        // Above threshold but not yet sustained → not a candidate.
        assert!(!is_candidate(&c, 900, 10, false, false, false, false));
        // Sustained but below threshold → not a candidate.
        assert!(!is_candidate(&c, 700, 60, false, false, false, false));
        // Both met (exactly at the boundaries) → candidate.
        assert!(is_candidate(&c, 800, 30, false, false, false, false));
        // Comfortably over both → candidate.
        assert!(is_candidate(&c, 990, 120, false, false, false, false));
    }

    #[test]
    fn disabled_never_dampens() {
        let mut c = cfg();
        c.enabled = false;
        assert!(!is_candidate(&c, 1000, 999, false, false, false, false));
    }

    #[test]
    fn foreground_is_never_a_candidate() {
        let c = cfg();
        // A raging hog, but it is the app the user is actively using.
        assert!(!is_candidate(&c, 1000, 300, false, true, false, false));
    }

    #[test]
    fn protected_is_never_a_candidate() {
        let c = cfg();
        // protected-critical / session-0 / system — excluded regardless of CPU.
        assert!(!is_candidate(&c, 1000, 300, true, false, false, false));
    }

    #[test]
    fn rule_governed_process_is_left_to_the_rule() {
        let c = cfg();
        // Precedence: an explicit rule already governs this process.
        assert!(!is_candidate(&c, 1000, 300, false, false, true, false));
    }

    #[test]
    fn no_double_dampen() {
        let c = cfg();
        // Already dampened this process — do not stack a second intervention.
        assert!(!is_candidate(&c, 1000, 300, false, false, false, true));
    }

    #[test]
    fn restore_on_calm_after_cooldown() {
        let c = cfg();
        // Below threshold for exactly the cool-down (== sustain) → restore.
        assert_eq!(
            restore_reason(&c, false, false, 40, Some(30)),
            Some(RestoreReason::Calmed)
        );
        // Below threshold but not long enough yet → keep dampening.
        assert_eq!(restore_reason(&c, false, false, 40, Some(29)), None);
        // Still at/above threshold (clock not running) and nothing else → keep.
        assert_eq!(restore_reason(&c, false, false, 40, None), None);
    }

    #[test]
    fn restore_on_max_duration_is_a_hard_cap() {
        let c = cfg();
        // Even while still pegged (below_secs None), the cap forces a restore.
        assert_eq!(
            restore_reason(&c, false, false, 300, None),
            Some(RestoreReason::MaxDuration)
        );
        // One second short of the cap, still pegged → keep.
        assert_eq!(restore_reason(&c, false, false, 299, None), None);
    }

    #[test]
    fn restore_on_becomes_foreground() {
        let c = cfg();
        assert_eq!(
            restore_reason(&c, false, true, 5, None),
            Some(RestoreReason::BecameForeground)
        );
    }

    #[test]
    fn restore_on_disabled_beats_everything() {
        let mut c = cfg();
        c.enabled = false;
        // Disabled outranks even a fresh, still-pegged dampening.
        assert_eq!(
            restore_reason(&c, false, false, 1, None),
            Some(RestoreReason::Disabled)
        );
    }

    #[test]
    fn restore_on_rule_takeover() {
        let c = cfg();
        assert_eq!(
            restore_reason(&c, true, false, 5, None),
            Some(RestoreReason::RuleGoverns)
        );
    }
}
