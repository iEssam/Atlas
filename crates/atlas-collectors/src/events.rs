//! Event-driven process lifecycle collection via ETW (tech-stack.md §4.1,
//! docs/phases.md M3).
//!
//! A user-mode ETW trace on the **Microsoft-Windows-Kernel-Process** provider
//! delivers exact process start/stop timestamps instead of the polling the
//! snapshot collector does — the "no polling for events" line of the idle-CPU
//! budget (tech-stack.md §10). Events are parsed off the ETW consumer thread
//! and pushed through a bounded channel to the caller.
//!
//! The ETW plumbing lives behind `ferrisetw` (the KrabsETW-inspired crate named
//! in tech-stack.md §3.1); this module only owns the provider config, the event
//! schema mapping, and the session lifecycle.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

use ferrisetw::native::EvntraceNativeError;
use ferrisetw::parser::Parser;
use ferrisetw::provider::Provider;
use ferrisetw::schema_locator::SchemaLocator;
use ferrisetw::trace::{stop_trace_by_name, TraceError, TraceTrait, UserTrace};
use ferrisetw::EventRecord;

/// Microsoft-Windows-Kernel-Process provider GUID (docs/phases.md M3).
const KERNEL_PROCESS_PROVIDER_GUID: &str = "22FB2CD6-0E7B-422B-A0C7-2FAD1FD0E716";

/// `WINEVENT_KEYWORD_PROCESS` — restricts the trace to process create/exit
/// events (and image loads), keeping the ETW volume tiny.
const PROCESS_KEYWORD: u64 = 0x10;

/// Manifest event ids on the Kernel-Process provider.
const EVENT_ID_PROCESS_START: u16 = 1;
const EVENT_ID_PROCESS_STOP: u16 = 2;

/// Depth of the channel between the ETW consumer thread and the caller. Process
/// churn is bursty (a build spawning hundreds of children); a few thousand slots
/// absorbs the burst, and anything beyond it is dropped rather than blocking the
/// consumer thread (the backpressure rule, tech-stack.md §4.1).
const CHANNEL_CAPACITY: usize = 4096;

/// FILETIME epoch (1601-01-01) offset from the Unix epoch, in 100 ns ticks.
const FILETIME_UNIX_EPOCH_DIFF_100NS: i64 = 116_444_736_000_000_000;

/// A single process lifecycle event with the ETW event's own timestamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEvent {
    /// Event timestamp as Unix epoch milliseconds (from the ETW record header,
    /// not our receive time).
    pub ts_ms: i64,
    pub pid: u32,
    pub kind: ProcessEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEventKind {
    Started {
        parent_pid: u32,
        session_id: u32,
        image_name: String,
    },
    Stopped {
        exit_status: i32,
    },
}

/// Errors starting or running the process event trace.
#[derive(Debug)]
pub enum EventError {
    /// Starting an ETW session requires administrative rights; the OS returned
    /// `ERROR_ACCESS_DENIED`. Callers should tell the user to rerun elevated.
    ElevationRequired,
    /// Any other failure setting up the trace (bad provider, name, native call).
    Trace(TraceError),
}

impl std::fmt::Display for EventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EventError::ElevationRequired => write!(
                f,
                "starting an ETW session requires elevation (run from an elevated terminal)"
            ),
            EventError::Trace(e) => write!(f, "ETW trace error: {e:?}"),
        }
    }
}

impl std::error::Error for EventError {}

impl From<TraceError> for EventError {
    fn from(err: TraceError) -> Self {
        // ERROR_ACCESS_DENIED from StartTraceW is the elevation signal. ferrisetw
        // builds the IoError from the failing `HRESULT.code()`, so in practice it
        // arrives as the HRESULT-wrapped form (0x80070005); accept the bare Win32
        // code (5) too for robustness across error paths.
        if let TraceError::EtwNativeError(EvntraceNativeError::IoError(io)) = &err {
            if is_access_denied(io.raw_os_error()) {
                return EventError::ElevationRequired;
            }
        }
        EventError::Trace(err)
    }
}

/// `ERROR_ACCESS_DENIED` from `<winerror.h>` (Win32) and its `HRESULT_FROM_WIN32`
/// form (`0x80070005`), which is what ferrisetw actually reports.
const ERROR_ACCESS_DENIED: i32 = 5;
const HRESULT_ACCESS_DENIED: i32 = 0x8007_0005_u32 as i32;

fn is_access_denied(code: Option<i32>) -> bool {
    matches!(
        code,
        Some(ERROR_ACCESS_DENIED) | Some(HRESULT_ACCESS_DENIED)
    )
}

/// A live process-event ETW session. Dropping it (or calling [`stop`]) tears the
/// session down; the paired [`Receiver`] returned by [`start`] then closes once
/// the consumer thread drains.
///
/// [`start`]: ProcessEventWatcher::start
/// [`stop`]: ProcessEventWatcher::stop
pub struct ProcessEventWatcher {
    trace: UserTrace,
    session_name: String,
    /// Number of events dropped because the channel was full.
    dropped: Arc<AtomicU64>,
}

impl ProcessEventWatcher {
    /// Start a user-mode ETW trace on Microsoft-Windows-Kernel-Process and
    /// return the watcher plus a receiver of parsed [`ProcessEvent`]s.
    ///
    /// Requires elevation; a non-elevated caller gets [`EventError::ElevationRequired`].
    pub fn start() -> Result<(Self, Receiver<ProcessEvent>), EventError> {
        let session_name = session_name();

        // A previous run that crashed without tearing down leaves the named
        // kernel session alive; StartTraceW would then fail with ALREADY_EXISTS.
        // Best-effort stop; ignore the (expected) error when none exists.
        let _ = stop_trace_by_name(&session_name);

        let (tx, rx) = sync_channel::<ProcessEvent>(CHANNEL_CAPACITY);
        let dropped = Arc::new(AtomicU64::new(0));

        let cb_tx = tx;
        let cb_dropped = Arc::clone(&dropped);
        let provider = Provider::by_guid(KERNEL_PROCESS_PROVIDER_GUID)
            .any(PROCESS_KEYWORD)
            .add_callback(
                move |record: &EventRecord, schema_locator: &SchemaLocator| {
                    if let Some(event) = parse_event(record, schema_locator) {
                        forward(&cb_tx, &cb_dropped, event);
                    }
                },
            )
            .build();

        // `start()` (not `start_and_process`) hands back both the trace and its
        // handle, so we keep the `UserTrace` for a clean `stop()` while running
        // the blocking processing loop on our own thread.
        let (trace, trace_handle) = UserTrace::new()
            .named(session_name.clone())
            .enable(provider)
            .start()?;

        std::thread::Builder::new()
            .name("atlas-etw-process".into())
            .spawn(move || {
                // Blocks until the trace is stopped/dropped, then returns.
                let _ = UserTrace::process_from_handle(trace_handle);
            })
            .map_err(|e| {
                EventError::Trace(TraceError::EtwNativeError(EvntraceNativeError::IoError(e)))
            })?;

        Ok((
            Self {
                trace,
                session_name,
                dropped,
            },
            rx,
        ))
    }

    /// The ETW session name (`SystemAtlas-Dev-<pid>`).
    pub fn session_name(&self) -> &str {
        &self.session_name
    }

    /// Number of events dropped so far because the channel was full.
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stop the ETW session cleanly, consuming the watcher.
    pub fn stop(self) -> Result<(), EventError> {
        self.trace.stop().map_err(EventError::from)
    }
}

/// Push an event to the caller without ever blocking the ETW consumer thread:
/// on a full channel we drop the event and bump the counter (tech-stack.md §4.1
/// backpressure rule — derived data may be dropped, a marker is recorded).
fn forward(tx: &SyncSender<ProcessEvent>, dropped: &AtomicU64, event: ProcessEvent) {
    match tx.try_send(event) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            dropped.fetch_add(1, Ordering::Relaxed);
        }
        // Receiver gone: the caller stopped listening; nothing to do.
        Err(TrySendError::Disconnected(_)) => {}
    }
}

/// Parse a Kernel-Process ETW record into a [`ProcessEvent`], or `None` for
/// events we do not model (image loads, thread events, unparseable records).
///
/// Property names are taken from the provider manifest: ProcessStart carries
/// `ProcessID`, `ParentProcessID`, `SessionID`, `ImageName`; ProcessStop carries
/// `ProcessID`, `ExitStatus`. The `ProcessID` payload field is the *subject*
/// process (the one starting/stopping), which is what we key on — not the ETW
/// header's `ProcessID`, which is the reporting process.
fn parse_event(record: &EventRecord, schema_locator: &SchemaLocator) -> Option<ProcessEvent> {
    let event_id = record.event_id();
    if event_id != EVENT_ID_PROCESS_START && event_id != EVENT_ID_PROCESS_STOP {
        return None;
    }

    let schema = schema_locator.event_schema(record).ok()?;
    let parser = Parser::create(record, &schema);
    let ts_ms = filetime_100ns_to_unix_ms(record.raw_timestamp());
    let pid: u32 = parser.try_parse("ProcessID").ok()?;

    let kind = match event_id {
        EVENT_ID_PROCESS_START => ProcessEventKind::Started {
            parent_pid: parser.try_parse("ParentProcessID").unwrap_or(0),
            session_id: parser.try_parse("SessionID").unwrap_or(0),
            image_name: parser.try_parse("ImageName").unwrap_or_default(),
        },
        EVENT_ID_PROCESS_STOP => ProcessEventKind::Stopped {
            exit_status: parser.try_parse("ExitStatus").unwrap_or(0),
        },
        _ => return None,
    };

    Some(ProcessEvent { ts_ms, pid, kind })
}

/// Convert an ETW record timestamp (FILETIME: 100 ns ticks since 1601-01-01) to
/// Unix epoch milliseconds. Ticks before the Unix epoch clamp to 0.
fn filetime_100ns_to_unix_ms(filetime_100ns: i64) -> i64 {
    let unix_100ns = filetime_100ns.saturating_sub(FILETIME_UNIX_EPOCH_DIFF_100NS);
    if unix_100ns <= 0 {
        return 0;
    }
    unix_100ns / 10_000
}

/// Per-process session name so concurrent dev runs don't collide on the ETW
/// session namespace.
fn session_name() -> String {
    format!("SystemAtlas-Dev-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_conversion_matches_known_epoch() {
        // The FILETIME epoch difference itself maps exactly to Unix time 0.
        assert_eq!(filetime_100ns_to_unix_ms(FILETIME_UNIX_EPOCH_DIFF_100NS), 0);
    }

    #[test]
    fn filetime_conversion_one_second_after_unix_epoch() {
        // 1 s == 10_000_000 ticks past the Unix epoch → 1000 ms.
        let one_sec = FILETIME_UNIX_EPOCH_DIFF_100NS + 10_000_000;
        assert_eq!(filetime_100ns_to_unix_ms(one_sec), 1000);
    }

    #[test]
    fn filetime_conversion_truncates_sub_ms() {
        // 1.5 ms past epoch (15_000 ticks) truncates to 1 ms.
        let val = FILETIME_UNIX_EPOCH_DIFF_100NS + 15_000;
        assert_eq!(filetime_100ns_to_unix_ms(val), 1);
    }

    #[test]
    fn filetime_before_unix_epoch_clamps_to_zero() {
        assert_eq!(filetime_100ns_to_unix_ms(0), 0);
        assert_eq!(
            filetime_100ns_to_unix_ms(FILETIME_UNIX_EPOCH_DIFF_100NS - 1),
            0
        );
    }

    #[test]
    fn session_name_is_per_process() {
        let name = session_name();
        assert!(name.starts_with("SystemAtlas-Dev-"));
        assert!(name.ends_with(&std::process::id().to_string()));
    }

    #[test]
    fn event_kinds_construct_as_expected() {
        let started = ProcessEvent {
            ts_ms: 1_700_000_000_000,
            pid: 1234,
            kind: ProcessEventKind::Started {
                parent_pid: 5678,
                session_id: 1,
                image_name: "notepad.exe".into(),
            },
        };
        match started.kind {
            ProcessEventKind::Started {
                parent_pid,
                session_id,
                ref image_name,
            } => {
                assert_eq!(parent_pid, 5678);
                assert_eq!(session_id, 1);
                assert_eq!(image_name, "notepad.exe");
            }
            _ => panic!("expected Started"),
        }
        assert_eq!(started.pid, 1234);
    }

    #[test]
    fn elevation_required_maps_from_access_denied() {
        // Both the bare Win32 code and the HRESULT form (what ferrisetw emits)
        // must be recognized as the elevation signal.
        for code in [ERROR_ACCESS_DENIED, HRESULT_ACCESS_DENIED] {
            let io = std::io::Error::from_raw_os_error(code);
            let err =
                EventError::from(TraceError::EtwNativeError(EvntraceNativeError::IoError(io)));
            assert!(
                matches!(err, EventError::ElevationRequired),
                "code {code:#x} should map to ElevationRequired"
            );
        }
    }

    #[test]
    fn other_native_errors_do_not_map_to_elevation() {
        let err = EventError::from(TraceError::InvalidTraceName);
        assert!(matches!(err, EventError::Trace(_)));
    }

    /// Live end-to-end trace. Ignored by default: it starts a real ETW session,
    /// which **requires an elevated (Administrator) terminal**. Run with
    /// `cargo test -p atlas-collectors -- --ignored process_event_watcher_live`.
    #[test]
    #[ignore = "requires elevation: starts a live ETW session"]
    fn process_event_watcher_live_start_stop() {
        use std::time::{Duration, Instant};

        let (watcher, rx) = ProcessEventWatcher::start().expect("start (run elevated)");

        // Spawn a short-lived child; expect a Started then Stopped for its pid.
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "exit", "0"])
            .spawn()
            .expect("spawn cmd");
        let child_pid = child.id();
        // Reap the child so its exit is real (and no zombie lingers) before we
        // wait on the ETW stream to report the stop.
        child.wait().expect("wait for child");

        let deadline = Instant::now() + Duration::from_secs(15);
        let mut saw_start = false;
        let mut saw_stop = false;
        while Instant::now() < deadline && !(saw_start && saw_stop) {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(ev) if ev.pid == child_pid => match ev.kind {
                    ProcessEventKind::Started { .. } => saw_start = true,
                    ProcessEventKind::Stopped { .. } => saw_stop = true,
                },
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        watcher.stop().expect("clean stop");
        assert!(saw_start, "expected a ProcessStart for pid {child_pid}");
        assert!(saw_stop, "expected a ProcessStop for pid {child_pid}");
    }
}
