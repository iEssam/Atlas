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

### M2 — Storage v0 `[x]` ✅ (completed 2026-07-13)

- [x] SQLite store (WAL, `synchronous=NORMAL`), schema v1, migrations via `PRAGMA user_version`
- [x] Batched writes: per-process aggregates (avg/max over flush window) + 1 Hz system samples, one transaction per flush (PRD §12.4: no per-sample writes)
- [x] Process instance registry with first/last/exit timestamps (PID-reuse-safe identity)
- [x] 72 h retention sweep (PRD §9.3.1 default)
- [x] Dev verification commands: `record`, `db-top`
- [x] Writer thread + bounded channel with backpressure: stalls drop the window and record a `gap_event` row (schema v2); window aggregates time-weighted for variable cadence
- [x] **M-TSDB:** Gorilla-compressed sample blocks (delta-of-delta ts + XOR values, CRC-framed) stored as SQLite BLOBs; raw per-tick T0 resolution; measured 1.73 B/sample live → ~215 MB/day steady-state (vs ~520 interim). T1/T2 roll-up tiers + file-based chunk store remain future work (closes the gap to ~150 MB/day)

### M3 — Process events & application grouping `[~]`

- [x] ETW session: `Microsoft-Windows-Kernel-Process` ProcessStart/ProcessStop via `ferrisetw` 1.2.0 (`ProcessEventWatcher` + `events` subcommand; elevation-aware). Live path covered by an `#[ignore]` test — needs one elevated validation run
- [x] Image-load events (same provider, id 5, keyword 0x40) — opt-in via `WatcherOptions` / `events --images`
- [x] Process lifecycle wired into `record`: event-driven wake (recv_timeout), `proc_event` table + `process_instance.exit_status` (schema v3), exact exit stamping by pid against live instances; clean degraded mode when not elevated. Command lines still unavailable from this provider — revisit with rundown/NT kernel logger later
- [ ] Application identity & grouping heuristics: main/renderer/helper/service roles (PRD §9.2.1) — deferred to pair with the M5 UI, which is its first consumer
- [x] Overhead harness (`overhead --duration N`): runs the real record pipeline against a temp db, grades own CPU/WS against PRD budgets, reports tick timings + disk extrapolation + ETW live/degraded. Baseline 2026-07-13: 0.03% CPU avg, ~13 MB WS (PASS); disk ~520 MB/day (see M-TSDB). Becomes a CI gate at M9

### M4 — IPC contract `[x]` ✅ (completed 2026-07-13)

- [x] Compile [proto/atlas.proto](../proto/atlas.proto): `atlas-ipc` crate, tonic 0.13 + prost 0.13, hermetic protoc via `protoc-bin-vendored`; named-pipe accept loop (always one listener pending) + client connector with pipe-busy retry; `serve` / `client-snapshot [--watch]` subcommands; unprivileged end-to-end round-trip test
- [x] Pipe security: full SDDL DACL — SYSTEM + Administrators full control, current user RW, nobody else (`atlas-ipc/src/security.rs`); per-connection client PID/signature auth noted as future hardening
- [x] Shared-memory live ring (seqlock, `Local\SystemAtlas.metrics.<disc>`, 64 top rows): `serve` publishes at 1 Hz; `ring-read [--watch]` dev reader; lock-free bounded-retry reads — the future emergency-UI path
- [x] Capability flags in `GetCapabilities` (currently: `process_snapshots`)

### M5 — UI shell (WinUI 3) `[~]`

- [x] .NET 10.0.301 SDK installed; `src-ui/Atlas.sln` — Atlas.IpcClient (gRPC-over-pipe via ConnectCallback, proto codegen, 15 tests), Atlas.DevCli (console interop proof), Atlas.App (WinUI 3, Windows App SDK 1.6, unpackaged; builds with PackageReferences only — no VS workloads needed)
- [x] Live Activity page: NavigationView + Mica shell, virtualized process table updated in place at ~1 Hz from StreamSnapshots; verified running against `serve` at ~181 MB WS (budget < 200 MB)
- [x] C# `MetricsRing` seqlock reader over the shared-mem ring (layout offsets pinned by tests against shm.rs); Live Activity + Overview prefer the ring, fall back to the gRPC stream, re-probe every ~15 s. Ring rows carry no thread/handle counts (layout v1) — those columns are stream-only
- [x] Overview page v0 (gauge cards + top-5 consumers, measured values only)
- [ ] NativeAOT publish profile — blocked on MVVMTK0045 (see decision note)

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
- **2026-07-13 — tonic pinned to 0.13** (not 0.14): 0.14 reworked the transport/`serve_with_incoming` surface; 0.13 has the stable `Connected` API the named-pipe transport builds on. protoc is vendored (`protoc-bin-vendored`) so builds are hermetic — no system protoc install.
- **2026-07-13 — Exit-stamping matches by pid, not (pid, create_time)**: ETW Stop events carry the stop time, never the snapshot's CreateTime, so the identity key cannot be reconstructed; stamping targets the unique live (`exit_seen_ms IS NULL`) instance per pid. Documented on `stamp_exit_by_pid`.
- **2026-07-13 — WinUI 3 builds under plain `dotnet` CLI**: no VS install or `dotnet workload` needed — Microsoft.WindowsAppSDK 1.6.250205002 + Microsoft.Windows.SDK.BuildTools PackageReferences with `<WindowsPackageType>None</WindowsPackageType>` (unpackaged). Do NOT add a custom `Program.cs` (the generated Main handles unpackaged bootstrap; `DisableXamlGeneratedMain` misbehaves under this SDK).
- **2026-07-13 — NativeAOT for the UI is blocked** on CommunityToolkit.Mvvm 8.4.0: field-based `[ObservableProperty]` trips MVVMTK0045 (WinRT AOT marshalling), and the partial-property form fails codegen under this SDK (CS9248/CS8050). Suppressed via NoWarn for now; revisit with a newer toolkit before the M9 AOT/perf gates.
- **2026-07-13 — Overhead baseline**: record pipeline 0.03% CPU avg / ~13 MB WS (both PASS); interim SQLite samples extrapolate to ~520 MB/day of disk writes vs the ~150 MB/day target — the M-TSDB Gorilla store is the planned fix and the harness now measures the before/after.
- **2026-07-13 — sample_block wire format (ATB1)**: `magic "ATB1" | count u32 | start_ms i64 | end_ms i64 | bitlen u32 | bitstream | crc32`, little-endian frame; bitstream MSB-first — point 0 raw (64+64 bits); timestamps delta-of-delta with Gorilla bucket codes (`0`, `10`+7b, `110`+9b, `1110`+12b, `1111`+64b, zigzagged); values Gorilla XOR (`0` unchanged, `10` reuse window, `11`+5b leading+6b length). CRC-32 poly 0xEDB88320 hand-rolled. Duplicate timestamps kept; backwards timestamps rejected without wedging the head; corrupt blocks are typed errors, never panics.
- **2026-07-13 — Blocks live in SQLite BLOBs, not chunk files** (maintainer decision): same compression win, SQLite keeps atomicity + retention trivial; `atlas-tsdb` stays byte-oriented (no rusqlite dep) so the tech-stack §4.2 file-based chunk store can swap in underneath the same API when tiering lands.
- **2026-07-13 — App Control now also blocks freshly built UI exes**: this round's `Atlas.App.exe` was blocked from launching (error 4551) though the previous round's build ran. Local UI launch testing is unreliable until the policy exempts the repo's build outputs; data paths are verified via tests + the ring/DevCli harnesses instead.
- **2026-07-13 — Machine policy blocks fresh build-script binaries**: an Application Control policy (error 4551) blocks executing freshly compiled build scripts (seen with `zmij`, a `serde_json` transitive build-dep) in fresh target dirs. Cached artifacts in the main `target/` are approved. Implications: full `cargo clean` or CI on this machine may need policy approval; and **never share a target dir between a worktree and the main repo** — doing so overwrote main's crate artifacts with worktree-era ones and broke the post-merge build until `cargo clean -p <workspace crates>`.
