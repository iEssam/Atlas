//! Adaptive sampling cadence (tech-stack.md §4.1, PRD §13.4).
//!
//! Pure decision logic: no clocks, no I/O. The caller measures elapsed
//! wall time and observed activity each tick and asks the controller what
//! the next sampling interval should be. Keeping this side-effect-free is
//! what makes the decay/return-on-activity policy unit-testable without
//! sleeping in tests.
//!
//! Budget rationale (PRD §12.2, §12.4): a fixed 1 s cadence burns the idle
//! CPU budget (< 0.2%) and writes far more sample windows than an idle
//! machine needs. So we hold 1 s only while something is actually moving
//! and decay toward coarse intervals during sustained quiet, always
//! snapping back to 1 s the instant activity reappears — coarse sampling
//! must never hide the onset of a spike.

use std::time::Duration;

/// Active cadence: the fast sampling floor while the system is busy or
/// activity was just observed.
pub const ACTIVE_INTERVAL: Duration = Duration::from_secs(1);
/// First idle tier: reached after ~30 s of continuous quiet.
pub const IDLE_INTERVAL_1: Duration = Duration::from_secs(5);
/// Second idle tier: reached after a further ~2 min of continuous quiet.
pub const IDLE_INTERVAL_2: Duration = Duration::from_secs(15);

/// Quiet threshold for *system* CPU, in permille (0..=1000). ~10% — below
/// this a machine is doing background housekeeping at most, not work a user
/// would notice, so it is safe to slow the sampler.
pub const QUIET_SYS_CPU_PERMILLE: u32 = 100;
/// Quiet threshold for any *single* process, in permille. ~5% — one process
/// climbing above this is the kind of activity worth 1 s resolution even if
/// the system total still looks calm (e.g. a single-core-bound task on a
/// many-core box).
pub const QUIET_PROC_CPU_PERMILLE: u32 = 50;

/// Continuous quiet time before decaying from active to the first idle tier.
pub const DECAY_TO_TIER1_AFTER: Duration = Duration::from_secs(30);
/// Additional continuous quiet time (beyond tier 1) before decaying to the
/// second idle tier — i.e. ~30 s + ~2 min total from the start of quiet.
pub const DECAY_TO_TIER2_AFTER: Duration = Duration::from_secs(150);

/// Observations for a single tick, fed to [`CadenceController::next_interval`].
#[derive(Debug, Clone, Copy)]
pub struct Tick {
    /// System-wide CPU utilization this tick, in permille (0..=1000).
    pub sys_cpu_permille: u32,
    /// Number of processes that started this tick (any start is activity).
    pub started: u32,
    /// Number of processes that exited this tick (any exit is activity).
    pub exited: u32,
    /// Highest per-process CPU utilization this tick, in permille.
    pub max_proc_cpu_permille: u32,
    /// Wall-clock time elapsed since the previous tick.
    pub elapsed: Duration,
}

impl Tick {
    /// A tick is "quiet" only when *every* activity signal is calm: system
    /// CPU below threshold, no process starts/exits, and no single process
    /// above its threshold. Any one signal firing makes the tick active.
    fn is_quiet(&self) -> bool {
        self.sys_cpu_permille < QUIET_SYS_CPU_PERMILLE
            && self.started == 0
            && self.exited == 0
            && self.max_proc_cpu_permille < QUIET_PROC_CPU_PERMILLE
    }
}

/// Stateful cadence decision engine. Holds only how long the system has been
/// continuously quiet; every input arrives via [`Tick`].
#[derive(Debug, Clone)]
pub struct CadenceController {
    /// Accumulated continuous quiet time. Reset to zero on any active tick.
    quiet_for: Duration,
    /// The interval chosen after the most recent tick.
    current: Duration,
}

impl Default for CadenceController {
    fn default() -> Self {
        Self::new()
    }
}

impl CadenceController {
    pub fn new() -> Self {
        Self {
            quiet_for: Duration::ZERO,
            current: ACTIVE_INTERVAL,
        }
    }

    /// The interval chosen after the most recent tick.
    pub fn current(&self) -> Duration {
        self.current
    }

    /// Fold one tick of observations in and return the interval to wait
    /// before the next sample. Activity snaps instantly back to 1 s; quiet
    /// accumulates and decays through the idle tiers.
    pub fn next_interval(&mut self, tick: Tick) -> Duration {
        if tick.is_quiet() {
            self.quiet_for = self.quiet_for.saturating_add(tick.elapsed);
        } else {
            self.quiet_for = Duration::ZERO;
        }

        self.current = if self.quiet_for >= DECAY_TO_TIER2_AFTER {
            IDLE_INTERVAL_2
        } else if self.quiet_for >= DECAY_TO_TIER1_AFTER {
            IDLE_INTERVAL_1
        } else {
            ACTIVE_INTERVAL
        };
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy_tick() -> Tick {
        Tick {
            sys_cpu_permille: 500,
            started: 0,
            exited: 0,
            max_proc_cpu_permille: 300,
            elapsed: Duration::from_secs(1),
        }
    }

    fn quiet_tick(elapsed: Duration) -> Tick {
        Tick {
            sys_cpu_permille: 20,
            started: 0,
            exited: 0,
            max_proc_cpu_permille: 10,
            elapsed,
        }
    }

    #[test]
    fn stays_active_under_load() {
        let mut c = CadenceController::new();
        for _ in 0..300 {
            assert_eq!(c.next_interval(busy_tick()), ACTIVE_INTERVAL);
        }
    }

    #[test]
    fn decays_through_both_idle_tiers() {
        let mut c = CadenceController::new();
        // Under 30 s of quiet: still active.
        assert_eq!(
            c.next_interval(quiet_tick(Duration::from_secs(29))),
            ACTIVE_INTERVAL
        );
        // Crossing 30 s: first idle tier.
        assert_eq!(
            c.next_interval(quiet_tick(Duration::from_secs(1))),
            IDLE_INTERVAL_1
        );
        // Still short of 150 s total: stays at tier 1.
        assert_eq!(
            c.next_interval(quiet_tick(Duration::from_secs(100))),
            IDLE_INTERVAL_1
        );
        // Crossing 150 s total: second idle tier.
        assert_eq!(
            c.next_interval(quiet_tick(Duration::from_secs(20))),
            IDLE_INTERVAL_2
        );
    }

    #[test]
    fn instant_return_to_active_on_system_cpu() {
        let mut c = deeply_idle();
        let mut t = quiet_tick(Duration::from_secs(1));
        t.sys_cpu_permille = QUIET_SYS_CPU_PERMILLE; // at/above threshold = active
        assert_eq!(c.next_interval(t), ACTIVE_INTERVAL);
    }

    #[test]
    fn instant_return_to_active_on_process_start() {
        let mut c = deeply_idle();
        let mut t = quiet_tick(Duration::from_secs(1));
        t.started = 1;
        assert_eq!(c.next_interval(t), ACTIVE_INTERVAL);
    }

    #[test]
    fn instant_return_to_active_on_process_exit() {
        let mut c = deeply_idle();
        let mut t = quiet_tick(Duration::from_secs(1));
        t.exited = 1;
        assert_eq!(c.next_interval(t), ACTIVE_INTERVAL);
    }

    #[test]
    fn instant_return_to_active_on_single_hot_process() {
        let mut c = deeply_idle();
        let mut t = quiet_tick(Duration::from_secs(1));
        t.max_proc_cpu_permille = QUIET_PROC_CPU_PERMILLE; // at/above threshold
        assert_eq!(c.next_interval(t), ACTIVE_INTERVAL);
    }

    #[test]
    fn thresholds_are_exclusive_lower_bounds() {
        // Just under each threshold is still quiet.
        let mut c = CadenceController::new();
        let mut t = quiet_tick(Duration::from_secs(30));
        t.sys_cpu_permille = QUIET_SYS_CPU_PERMILLE - 1;
        t.max_proc_cpu_permille = QUIET_PROC_CPU_PERMILLE - 1;
        assert_eq!(c.next_interval(t), IDLE_INTERVAL_1);
    }

    /// A controller driven far into the second idle tier.
    fn deeply_idle() -> CadenceController {
        let mut c = CadenceController::new();
        assert_eq!(
            c.next_interval(quiet_tick(DECAY_TO_TIER2_AFTER)),
            IDLE_INTERVAL_2
        );
        c
    }
}
