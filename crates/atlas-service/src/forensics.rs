//! R3 forensics background detectors (docs/phases.md Phase 3, PRD §9.13/§9.14).
//!
//! Two self-driving detector threads run inside `serve` alongside the sampler and
//! the privacy watcher/evaluator, joined on shutdown:
//!
//! * [`ChangeDetector`] — the system-change tracker (PRD §9.13). On each pass it
//!   collects the current app/service/startup/task/power/default-app inventory,
//!   diffs it against the persisted baseline (the reliable, unprivileged core —
//!   catches changes with no event trail), records the differences as
//!   `system_change` rows, and rewrites the baseline. It additionally imports WUA
//!   Windows-Update history once per process start (event-sourced augmentation,
//!   de-duplicated against what's already recorded).
//!
//! * [`CrashScanner`] — the crash/reliability correlator (PRD §9.14). On a slower
//!   cadence it reads WER app crashes/hangs, service failures, bugchecks and
//!   unexpected shutdowns from the event logs, assembles a FACTUAL, hedged
//!   `context` list around each event from what the store already knows (peak
//!   CPU/memory in the minutes before, system changes in the prior 24 h,
//!   repeated-restart pattern), and records `crash_record` rows. Correlation is
//!   not causation — every context line says so.
//!
//! The pure logic (`diff_inventories`, `count_repeated_restarts`,
//! `recent_change_notes`) lives in `atlas-collectors` and is unit-tested there;
//! this module is the store-touching driver only.

#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use atlas_collectors::changes::{self, change_kind, DetectedChange, Inventory};
use atlas_collectors::crashes::{self, count_repeated_restarts, recent_change_notes, RawCrash};
use atlas_store::{CrashRow, SystemChangeRow};
use atlas_tsdb::Metric;

use crate::ipc::SharedStore;

/// The store key under which the change-detector persists its "last inventory"
/// baseline (one JSON blob covering every inventory kind).
const INVENTORY_KEY: &str = "full";

/// The store key under which the crash-scanner persists its last availability
/// result (so `ListCrashes` can report `available`/`unavailable_reason` without
/// re-reading the event log on every call).
const CRASH_STATUS_KEY: &str = "crash_scan_status";

/// Change-detection cadence (§9.13): frequent enough to attribute "what changed
/// just before the problem", cheap because a quiet box diffs to nothing.
const CHANGE_INTERVAL: Duration = Duration::from_secs(60);

/// Crash-scan cadence (§9.14): the reliability logs move slowly, so a coarser
/// refresh keeps the event-log reads off the hot path.
const CRASH_INTERVAL: Duration = Duration::from_secs(300);

/// Granularity of the interruptible sleep between passes — small so shutdown is
/// observed promptly regardless of the (much longer) pass interval.
const TICK: Duration = Duration::from_millis(200);

/// Crash correlation look-back for the resource-context window (peak CPU/memory).
const RESOURCE_WINDOW_MS: i64 = 5 * 60_000;

/// Crash correlation look-back for "recent system changes before the crash".
const CHANGE_WINDOW_MS: i64 = 24 * 60 * 60_000;

/// Crash correlation window for repeated-restart detection.
const RESTART_WINDOW_MS: i64 = 60 * 60_000;

/// How far back the crash scanner reads the event logs on each pass.
const CRASH_LOOKBACK_MS: i64 = 30 * 24 * 60 * 60_000;

/// Milliseconds since the Unix epoch (local helper; the store's is private).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Runs `body` in `TICK` steps until either `total` has elapsed or `stop` is set.
/// Returns `false` when interrupted by shutdown (so the caller stops promptly).
fn sleep_interruptible(total: Duration, stop: &AtomicBool) -> bool {
    let mut waited = Duration::ZERO;
    while waited < total {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep(TICK);
        waited += TICK;
    }
    !stop.load(Ordering::SeqCst)
}

/// Maps a `SystemChangeKind` discriminant to a short snake-case label used in
/// correlation notes (e.g. "app_updated").
fn change_kind_label(kind: i32) -> String {
    let s = match kind {
        change_kind::APP_INSTALLED => "app_installed",
        change_kind::APP_UPDATED => "app_updated",
        change_kind::APP_REMOVED => "app_removed",
        change_kind::DRIVER_INSTALLED => "driver_installed",
        change_kind::DRIVER_UPDATED => "driver_updated",
        change_kind::WINDOWS_UPDATE => "windows_update",
        change_kind::SERVICE_INSTALLED => "service_installed",
        change_kind::SERVICE_CONFIG_CHANGED => "service_config_changed",
        change_kind::SERVICE_REMOVED => "service_removed",
        change_kind::STARTUP_ADDED => "startup_added",
        change_kind::STARTUP_REMOVED => "startup_removed",
        change_kind::SCHEDULED_TASK_ADDED => "scheduled_task_added",
        change_kind::SCHEDULED_TASK_REMOVED => "scheduled_task_removed",
        change_kind::POWER_PLAN_CHANGED => "power_plan_changed",
        change_kind::DEFAULT_APP_CHANGED => "default_app_changed",
        _ => "change",
    };
    s.to_string()
}

/// Builds a `SystemChangeRow` from a detected change, stamped at `ts_ms`.
fn change_to_row(d: &DetectedChange, ts_ms: i64) -> SystemChangeRow {
    SystemChangeRow {
        id: 0,
        ts_ms,
        kind: d.kind,
        subject: d.subject.clone(),
        detail: d.detail.clone(),
        publisher: d.publisher.clone(),
        responsible: d.responsible.clone(),
        reversible: d.reversible,
    }
}

/// The peak value across decoded blocks whose samples fall within `[from, to]`,
/// or `None` when the window has no samples.
fn peak_in_window(blocks: &[atlas_store::DecodedBlock], from: i64, to: i64) -> Option<f64> {
    let mut peak: Option<f64> = None;
    for b in blocks {
        for &(ts, v) in &b.points {
            if ts < from || ts > to {
                continue;
            }
            peak = Some(peak.map_or(v, |p: f64| p.max(v)));
        }
    }
    peak
}

// ---------------------------------------------------------------------------
// System-change detector (§9.13)
// ---------------------------------------------------------------------------

/// The periodic inventory-diff change detector. Owns a shared store handle.
pub struct ChangeDetector {
    store: SharedStore,
}

impl ChangeDetector {
    pub fn new(store: SharedStore) -> Self {
        Self { store }
    }

    /// Detector loop: seed + diff once immediately (so a fresh box records its
    /// baseline and a restart picks up anything that changed while stopped), then
    /// diff every [`CHANGE_INTERVAL`] until `stop` is set.
    pub fn run(self, stop: Arc<AtomicBool>) {
        // First pass imports WUA history (deduped) and seeds the baseline.
        let _ = self.detect_once(true);
        while sleep_interruptible(CHANGE_INTERVAL, &stop) {
            let _ = self.detect_once(false);
        }
    }

    /// One detection pass: collect the live inventory, diff it against the stored
    /// baseline, record the differences, then rewrite the baseline. Returns the
    /// number of `system_change` rows recorded. When `import_updates` is set it
    /// also imports WUA Windows-Update history (deduped). Used both by the loop
    /// and by the `detect-changes` dev command.
    pub fn detect_once(&self, import_updates: bool) -> usize {
        let next = changes::collect_inventory();
        let baseline = self.load_baseline();
        let now = now_ms();
        let mut recorded = 0;

        if let Some(prev) = baseline {
            let diffs = changes::diff_inventories(&prev, &next);
            if !diffs.is_empty() {
                if let Ok(store) = self.store.lock() {
                    for d in &diffs {
                        if store.record_system_change(&change_to_row(d, now)).is_ok() {
                            recorded += 1;
                        }
                    }
                }
            }
        }
        // else: first-ever pass on this db — seed the baseline only, no diff.

        // Persist the new baseline for the next pass.
        self.save_baseline(&next);

        if import_updates {
            recorded += self.import_windows_updates(now);
        }
        recorded
    }

    /// Reads and deserializes the persisted inventory baseline, or `None` when no
    /// baseline exists yet (a malformed blob is treated as absent so the next pass
    /// re-seeds rather than failing).
    fn load_baseline(&self) -> Option<Inventory> {
        let json = self
            .store
            .lock()
            .ok()?
            .get_inventory(INVENTORY_KEY)
            .ok()??;
        serde_json::from_str(&json).ok()
    }

    /// Serializes and persists the inventory baseline (best-effort; a serialize or
    /// store failure just means the next pass re-seeds).
    fn save_baseline(&self, inv: &Inventory) {
        if let Ok(json) = serde_json::to_string(inv) {
            if let Ok(store) = self.store.lock() {
                let _ = store.set_inventory(INVENTORY_KEY, &json);
            }
        }
    }

    /// Imports WUA Windows-Update history, recording entries not already present
    /// (deduped by subject+detail against recorded WINDOWS_UPDATE rows). Returns
    /// the number of new rows. WUA is best-effort: an unavailable agent yields no
    /// rows and no error.
    fn import_windows_updates(&self, now: i64) -> usize {
        let history = changes::windows_update_history(200);
        if history.is_empty() {
            return 0;
        }
        // Existing WINDOWS_UPDATE rows to dedupe against (this process may restart
        // over a db that already imported history).
        let existing: std::collections::HashSet<(String, String)> = match self.store.lock() {
            Ok(store) => store
                .list_system_changes(0, i64::MAX, &[change_kind::WINDOWS_UPDATE], 1000)
                .map(|(rows, _)| rows.into_iter().map(|r| (r.subject, r.detail)).collect())
                .unwrap_or_default(),
            Err(_) => return 0,
        };

        let mut recorded = 0;
        if let Ok(store) = self.store.lock() {
            for d in &history {
                if existing.contains(&(d.subject.clone(), d.detail.clone())) {
                    continue;
                }
                if store.record_system_change(&change_to_row(d, now)).is_ok() {
                    recorded += 1;
                }
            }
        }
        recorded
    }
}

// ---------------------------------------------------------------------------
// Crash / reliability correlator (§9.14)
// ---------------------------------------------------------------------------

/// The periodic crash/reliability scanner + correlator. Owns a shared store.
pub struct CrashScanner {
    store: SharedStore,
}

impl CrashScanner {
    pub fn new(store: SharedStore) -> Self {
        Self { store }
    }

    /// Scanner loop: scan once immediately, then every [`CRASH_INTERVAL`] until
    /// `stop` is set.
    pub fn run(self, stop: Arc<AtomicBool>) {
        let _ = self.scan_once();
        while sleep_interruptible(CRASH_INTERVAL, &stop) {
            let _ = self.scan_once();
        }
    }

    /// One scan pass: read the reliability/WER logs, persist the availability
    /// result, then for each crash assemble the correlation context and record it.
    /// Returns the number of crash rows recorded/refreshed.
    pub fn scan_once(&self) -> usize {
        let now = now_ms();
        let scan = crashes::read_crashes(now - CRASH_LOOKBACK_MS, 500);

        // Persist availability so ListCrashes can answer honestly without a live
        // event-log read on every call.
        let status = serde_json::json!({
            "available": scan.available,
            "reason": scan.unavailable_reason,
        })
        .to_string();
        if let Ok(store) = self.store.lock() {
            let _ = store.set_inventory(CRASH_STATUS_KEY, &status);
        }

        if !scan.available {
            return 0;
        }

        let mut recorded = 0;
        for c in &scan.crashes {
            let context = self.correlate(c, &scan.crashes);
            let row = CrashRow {
                id: 0,
                ts_ms: c.ts_ms,
                kind: c.kind,
                subject: c.subject.clone(),
                fault: c.fault.clone(),
                exception_code: c.exception_code.clone(),
                context,
            };
            if let Ok(store) = self.store.lock() {
                if store.record_crash(&row).is_ok() {
                    recorded += 1;
                }
            }
        }
        recorded
    }

    /// Assembles the FACTUAL, hedged correlation context for one crash from what
    /// the store already knows around its timestamp. Each lock is taken and
    /// released independently (no lock is held across a store call) to avoid any
    /// re-entrant lock. `all` is the full crash batch for repeated-restart counts.
    fn correlate(&self, c: &RawCrash, all: &[RawCrash]) -> Vec<String> {
        let mut ctx = Vec::new();
        let win_start = c.ts_ms - RESOURCE_WINDOW_MS;

        // Peak system CPU in the minutes before.
        if let Some(peak) = self.peak(Metric::SysCpuPermille, win_start, c.ts_ms) {
            ctx.push(format!(
                "peak system CPU {:.0}% in the 5 min before this event (correlation, not proof)",
                peak / 10.0
            ));
        }
        // Peak system memory used in the minutes before.
        if let Some(peak) = self.peak(Metric::SysMemUsed, win_start, c.ts_ms) {
            ctx.push(format!(
                "peak system memory {:.1} GB used in the 5 min before this event (correlation, not proof)",
                peak / 1_000_000_000.0
            ));
        }
        if c.kind == atlas_collectors::crashes::crash_kind::GPU_DRIVER_RESET {
            if let Some(peak) = self.peak(Metric::SysGpuPermille, win_start, c.ts_ms) {
                ctx.push(format!(
                    "peak GPU activity {:.0}% in the 5 min before this reset (correlation, not proof)",
                    peak / 10.0
                ));
            }
            if let Some(peak) = self.peak(Metric::SysGpuMemoryUsed, win_start, c.ts_ms) {
                ctx.push(format!(
                    "peak measured graphics memory {:.1} GB in the 5 min before this reset (correlation, not proof)",
                    peak / 1_000_000_000.0
                ));
            }
        }

        // System changes in the 24 h before the crash.
        let changes = self.recent_changes(c.ts_ms);
        ctx.extend(recent_change_notes(&changes, c.ts_ms, CHANGE_WINDOW_MS));

        // Repeated-restart pattern for the same subject.
        let repeats = count_repeated_restarts(all, &c.subject, c.ts_ms, RESTART_WINDOW_MS);
        if repeats >= 3 {
            ctx.push(format!(
                "'{}' appears {} times in the hour up to this event (repeated-restart pattern; correlation, not proof)",
                c.subject, repeats
            ));
        }

        ctx
    }

    /// Peak of a system-scoped metric within `[from, to]`, or `None`.
    fn peak(&self, metric: Metric, from: i64, to: i64) -> Option<f64> {
        let store = self.store.lock().ok()?;
        let blocks = store
            .read_blocks(metric, Some(atlas_tsdb::SYSTEM_SCOPE), from, to)
            .ok()?;
        peak_in_window(&blocks, from, to)
    }

    /// Recorded system changes in the 24 h before `crash_ms`, shaped as the
    /// `(ts_ms, kind_label, subject)` tuples `recent_change_notes` consumes.
    fn recent_changes(&self, crash_ms: i64) -> Vec<(i64, String, String)> {
        let store = match self.store.lock() {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        store
            .list_system_changes(crash_ms - CHANGE_WINDOW_MS, crash_ms, &[], 50)
            .map(|(rows, _)| {
                rows.into_iter()
                    .map(|r| (r.ts_ms, change_kind_label(r.kind), r.subject))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Reads the persisted crash-scan availability `(available, reason)` for the
/// `ListCrashes` reply. Defaults to available (the store is readable) when the
/// scanner has not recorded a status yet — the scanner runs once at startup so
/// this is only the brief pre-seed window.
pub fn crash_availability(store: &SharedStore) -> (bool, String) {
    let json = match store.lock() {
        Ok(s) => s.get_inventory(CRASH_STATUS_KEY).ok().flatten(),
        Err(_) => None,
    };
    match json.and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok()) {
        Some(v) => {
            let available = v.get("available").and_then(|a| a.as_bool()).unwrap_or(true);
            let reason = v
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("")
                .to_string();
            (available, reason)
        }
        None => (true, String::new()),
    }
}
