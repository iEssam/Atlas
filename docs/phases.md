# System Atlas — Implementation Phases & Milestone Tracker

Living document. Update the checkboxes as work lands; add decision notes at the bottom.
References: [project.md](../project.md) (PRD), [tech-stack.md](../tech-stack.md) (technical design).

Legend: `[x]` done · `[~]` in progress · `[ ]` not started

---

## Phase 0 — Foundation ✅ (completed 2026-07-13)

- [x] PRD ([project.md](../project.md)) and tech design ([tech-stack.md](../tech-stack.md))
- [x] git repository, `.gitignore` / `.gitattributes`
- [x] Cargo workspace per tech-stack §9.1 (crates: collectors, store, tsdb, service)
- [x] Phase tracker (this file), README
- [x] CI workflow: `cargo fmt --check`, `clippy -D warnings`, `cargo test` (`.github/workflows/ci.yml`)
- [x] IPC contract sketch ([proto/atlas.proto](../proto/atlas.proto)) — codegen deferred to M4

**Exit criteria:** repo builds and tests green locally. ✅

---

## Phase 1 — MVP (PRD §18.1)

### M1 — Collection heartbeat `[x]` ✅ (completed 2026-07-13)

- [x] Process snapshot collector: single `NtQuerySystemInformation(SystemProcessInformation)` call per tick (tech-stack §4.1) — CPU times, cycle time, working set, private bytes, I/O totals, handles, threads, session, parent
- [x] System gauges: `GetSystemTimes` (CPU), `GlobalMemoryStatusEx` (memory/commit)
- [x] Sampler: per-process CPU‰ / IO-rate deltas, keyed by `(pid, create_time)` so PID reuse cannot corrupt attribution
- [x] Dev verification commands: `top`, `snapshot`
- [x] Adaptive cadence: 1 s active → 5 s → 15 s idle decay, instant return on activity (`CadenceController`, pure + unit-tested)
- [x] Self-metrics: `self_sample` per flush window (own CPU/WS + sampler tick timing), surfaced as the `db-top` overhead line (PRD §12.2)

### M2 — Storage v0 `[~]`

- [x] SQLite store (WAL, `synchronous=NORMAL`), schema v1, migrations via `PRAGMA user_version`
- [x] Batched writes: per-process aggregates (avg/max over flush window) + 1 Hz system samples, one transaction per flush (PRD §12.4: no per-sample writes)
- [x] Process instance registry with first/last/exit timestamps (PID-reuse-safe identity)
- [x] 72 h retention sweep (PRD §9.3.1 default)
- [x] Dev verification commands: `record`, `db-top`
- [x] Writer thread + bounded channel with backpressure: stalls drop the window and record a `gap_event` row (schema v2); window aggregates time-weighted for variable cadence
- [ ] **M-TSDB:** replace interim SQLite sample tables with the chunked Gorilla-compressed tiered store (tech-stack §4.2); keep SQLite for events/entities

### M3 — Process events & application grouping `[~]`

- [x] ETW session: `Microsoft-Windows-Kernel-Process` ProcessStart/ProcessStop via `ferrisetw` 1.2.0 (`ProcessEventWatcher` + `events` subcommand; elevation-aware). Live path covered by an `#[ignore]` test — needs one elevated validation run
- [ ] Image-load events (same provider)
- [ ] Exact process lifecycle from events (create/exit timestamps, command line where available) wired into the `record` pipeline
- [ ] Application identity & grouping heuristics: main/renderer/helper/service roles (PRD §9.2.1)
- [ ] ETW cost harness — measure collector overhead against the 0.2% idle budget (tech-stack §13 spike 2)

### M4 — IPC contract `[ ]`

- [ ] Compile [proto/atlas.proto](../proto/atlas.proto): `prost`/`tonic` (Rust), named-pipe server in service
- [ ] Pipe security: SDDL (SYSTEM + interactive user), message-mode, size caps
- [ ] Shared-memory live ring (seqlock) for 1 Hz top-N rows (tech-stack §5.1)
- [ ] Capability flags in `GetCapabilities` (degraded-mode propagation)

### M5 — UI shell (WinUI 3) `[ ]`

- [ ] .NET 10 + Windows App SDK solution (`src-ui/`), NativeAOT publish profile
- [ ] Live Activity: virtualized process table bound to shared-mem ring + gRPC detail
- [ ] Overview page v0 (gauges, top consumers)
- [ ] Requires: .NET SDK + Windows App SDK workload install

### M6 — Timeline v0, search, safe actions `[ ]`

- [ ] Timeline view over stored samples/events (zoom, hover, missing-data rendering)
- [ ] Global search (SQLite FTS5): name, path, PID, service (PRD §9.2.4 subset)
- [ ] Safe end-task flow: close-normally → suspend → terminate ladder with consent tokens (PRD §9.22); broker v0 policy + audit log
- [ ] Incident bookmarks with global hotkey (tray helper) (PRD §9.3.6)

### M7 — Privacy, startup, services `[ ]`

- [ ] ConsentStore watcher: camera/mic/location events with app attribution (PRD §9.10)
- [ ] Startup inventory: Run keys, Startup folders, StartupApproved, packaged StartupTask (PRD §9.8.1 core sources)
- [ ] Services inventory: SCM enumeration + `NotifyServiceStatusChange` (PRD §9.9.1)

### M8 — Incidents, detectors, reports `[ ]`

- [ ] Threshold+duration incident detectors (CPU saturation, memory pressure, disk latency) (PRD §9.3.7 subset)
- [ ] Diagnostic summary templates with confidence wording (PRD §9.15.2, no LLM required)
- [ ] Report export: HTML/CSV with redaction pass (PRD §9.18)

### M9 — Hardening & packaging `[ ]`

- [ ] Perf gates in CI: idle CPU, service RSS, cold start, disk writes/hour (tech-stack §10)
- [ ] Windows service mode (install/start/stop), crash-restart recovery
- [ ] WiX MSI installer; code signing setup
- [ ] Soak test: 72 h leak/slope check

**Phase 1 exit criteria:** PRD §20 items 1–6, 9–11, 13–17 demonstrable end-to-end.

---

## Phase 2 — R2 (PRD §18.2)

Deep inspector (handles/modules/threads on-demand snapshots) · Restart-Manager file locks + Explorer context-menu (sparse MSIX) · rules engine + profiles + simulation · boot analysis · scheduled tasks · full network inspector (kernel-network ETW + DNS) · battery/thermal (vendor GPU libs, SRUM) · before/after experiments · local AI ladder (ONNX Runtime GenAI → llama.cpp → BYO endpoint) with grounded tool-calling · advanced privacy alerts.

## Phase 3 — R3 (PRD §18.3)

Dynamic responsiveness protection · extended retention tiers + optional Parquet/DuckDB analytics sidecar · crash correlation depth · driver/system-change tracking completeness · CLI (`atlas`) + PowerShell module · signed out-of-proc plugin framework · remote support bundle · kernel-driver decision gate (tech-stack §4.9).

---

## Decision notes (ADR seeds)

- **2026-07-13 — Interim samples in SQLite.** Per-process samples are stored as *window aggregates* (avg/max over the flush interval) in SQLite until the chunked TSDB lands (M-TSDB). Bounds row growth (~1 row/process/15 s instead of 1/s) while keeping the end-to-end path real. The `atlas-tsdb` crate holds the target API shape.
- **2026-07-13 — Hand-written FFI in `atlas-collectors::ffi`.** The first slice needs ~5 functions with stable ABIs; owning the definitions keeps the entire unsafe surface reviewable in one file and avoids feature-name churn. Struct layouts are locked by offset tests. Migration to `windows-sys` planned when the collector set grows (M3, ETW).
- **2026-07-13 — Dev data lives outside the repo** (`%LOCALAPPDATA%\SystemAtlas\dev`): keeps OneDrive sync and git status clean; production location will be `%ProgramData%` (service) per tech-stack §7. (Repo itself moved to `C:\Projects\System Atlas` the same day.)
- **2026-07-13 — ferrisetw 1.2.0** for ETW. Gotchas encoded in `events.rs`: use `start()` + own processing thread (the convenience `start_and_process()` can't be stopped cleanly); access-denied surfaces as HRESULT `0x80070005`, not Win32 error 5 — match both; parse the payload `ProcessID` (subject), not the ETW header pid (reporter); `raw_timestamp()` is FILETIME 100 ns units.
- **2026-07-13 — Writer backpressure semantics**: a stalled writer drops whole flush windows (counted, then recorded as `gap_event` rows by the next landed batch) rather than blocking the sampling loop — degradation is observable, never silent (PRD §11.3).
