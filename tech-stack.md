# System Atlas — Technology Stack & Technical Design Document

**Companion document to:** [project.md](project.md) (Product Requirements Document)
**Document status:** Proposed target technical baseline v1.0
**Date:** 2026-07-12
**Scope:** Language and framework selection, per-component technology mapping, Windows API strategy, IPC design, storage design, security architecture, packaging, testing, and performance-budget engineering for the MVP through Release 3.

> This document includes target-state architecture and should not be read as an inventory of shipped components. See [docs/current-state.md](docs/current-state.md) for the current source-tree baseline and [docs/phases.md](docs/phases.md) for implementation and release-gate status. In particular, NativeAOT UI publishing, sparse-MSIX Explorer integration, ARM64 release artifacts, signed updates, and the full test matrix below remain partially or wholly deferred.

---

# 0. Stack at a Glance

| Layer | Technology | Primary reason |
|---|---|---|
| Collection service | **Rust** (`windows-rs`, `ferrisetw`/ETW, tokio) running as a Windows service | No GC pauses, tiny footprint, memory safety in the most privileged component |
| Local data engine | **SQLite (WAL)** for events/metadata + **custom Gorilla-compressed time-series store** (Rust) | Meets append-heavy, low-write-amplification, tiered-retention requirements |
| Rules engine | **Rust**, declarative JSON rule definitions evaluated in the service | Shared event stream, deterministic, auditable, rollback-friendly |
| Diagnostics engine | **Rust** (statistical baselines, changepoint detection, evidence graph) | Runs against the same store; confidence scoring per PRD §9.15 |
| Privileged operations | **In-service authenticated command broker** (Rust, LocalSystem) with per-action policy + audit | Minimal privileged surface per PRD §13.1 |
| Main UI | **WinUI 3 (Windows App SDK) + C# / .NET 10**, custom **Win2D** chart renderer | Genuine Windows 11 Fluent look, UIA accessibility for free, high-density custom charts |
| Emergency UI | **Rust + raw Win32** (single small exe, high priority) | Must work when the system is dying (PRD §12.7) |
| IPC | **Shared-memory ring buffer** (live metrics) + **gRPC over named pipes** (queries/commands, proto3) | Zero-copy hot path; typed, versioned, cross-language control plane |
| Natural-language analysis | **No hosted model.** In-app: deterministic template/playbook matching over the query API (local, always on). External: a read-only **MCP server** (`atlas-mcp`, Rust) exposing grounded query tools to the user's own MCP client (Claude/ChatGPT) | Atlas is the evidence provider; the user's client owns the model + conversation. Isolation per PRD §13.2, citation-ready grounding per PRD §9.16 |
| Shell integration | **IExplorerCommand** COM + sparse MSIX package | Windows 11 modern context menu ("Find what is using this file") |
| Kernel driver | **None in v1.** ETW covers events; GPU vendor libraries cover most sensors. Re-evaluate a minimal read-only sensor driver in v2 (PRD §13.6 checklist) | Avoid signing/security/compat cost until proven necessary |
| Installer / updates | **WiX (v5/v6) MSI** + winget, signed update manifest, staged channels | Service + Win32 app + sparse MSIX in one per-machine install |
| CLI / automation (R3) | Rust `atlas` CLI + thin PowerShell module over the same gRPC API | One API surface for UI, CLI, and plugins |

Two languages, one contract: **Rust for everything that runs forever, C# only for the UI**, with all cross-process contracts defined in protobuf and versioned.

---

# 1. How the PRD Constrains Technology Choices

Every stack decision below traces back to hard requirements in the PRD:

1. **Idle CPU < 0.2%, service RAM < 100 MB, UI RAM < 200 MB (§12).** Rules out Electron outright, makes GC-heavy or JIT-heavy background processes risky, and demands event-driven (ETW) collection instead of polling wherever possible.
2. **UI visible in 500 ms; usable at 100% CPU (§12.1, §12.7).** Demands a pre-running background service (UI is a thin viewer), compiled/AOT UI startup, a dedicated high-priority control path, and an emergency UI with near-zero dependencies.
3. **Deep inspection: handles, threads, tokens, modules, ETW events (§9.4).** Requires first-class access to Win32/NT native APIs — this is a native-Windows product; cross-platform abstraction layers (Qt, Flutter, web-first) subtract value.
4. **Process isolation: UI crash must not stop collection; an MCP-server failure must not affect monitoring (§13.2).** Requires a multi-process architecture with well-defined IPC, not a monolith.
5. **Local-first privacy, no account, optional read-only MCP integration (§3.6, §15, §9.16).** Requires an embedded local store (no server database); natural-language reasoning is delegated to the user's own MCP client, so Atlas ships no inference runtime.
6. **Native Windows 11 look, accessibility, keyboard-first, touch, high-DPI (§11).** WinUI 3 provides Fluent materials (Mica), Segoe Fluent iconography, and UI Automation accessibility as platform defaults rather than reimplementations.
7. **72-hour to 30-day history with second-level precision and low disk amplification (§9.3, §12.4).** Requires a purpose-built tiered time-series layout with batched, compressed writes — neither raw SQLite rows per sample nor a server TSDB fits.
8. **Safe, reversible, audited actions (§3.3, §14.5).** Requires a single privileged chokepoint (broker) that records before/after state for every mutation.

---

# 2. Architecture Overview

## 2.1 Process topology

```
┌─────────────────────────────── User session (standard user) ───────────────────────────────┐
│                                                                                             │
│  ┌───────────────────────────┐   ┌──────────────────┐   ┌───────────────────────────────┐  │
│  │ Atlas UI (WinUI 3, C#)    │   │ Atlas Tray Helper│   │ Atlas Emergency UI (Rust/Win32)│  │
│  │ dashboards, timeline,     │   │ hotkeys, toasts, │   │ spawned on demand, high prio,  │  │
│  │ inspector, reports        │   │ incident marker  │   │ kill/suspend only              │  │
│  └────────────┬──────────────┘   └────────┬─────────┘   └───────────────┬───────────────┘  │
│               │  shared-mem ring (read)   │ gRPC/named pipe             │ gRPC/named pipe  │
│               │  + gRPC/named pipe        │                             │                  │
│  ┌────────────┴──────────────┐   ┌────────┴─────────┐                                      │
│  │ atlas-mcp (Rust, opt-in)  │   │ Explorer shell   │                                      │
│  │ read-only MCP server;     │   │ ext (IExplorer-  │                                      │
│  │ MCP tools → AtlasQuery;   │   │ Command, sparse  │                                      │
│  │ hosts NO model — the      │   │ MSIX)            │                                      │
│  │ user's MCP client does    │   │                  │                                      │
│  └───────────────────────────┘   └──────────────────┘                                      │
└───────────────┬─────────────────────────────────────────────────────────────────────────── ┘
                │ authenticated gRPC over named pipe (ACL: interactive user + SYSTEM)
┌───────────────┴────────────────────────── LocalSystem ─────────────────────────────────────┐
│  Atlas Service (Rust, Windows service, auto-start, crash-restart)                           │
│  ┌──────────────┐ ┌─────────────┐ ┌──────────────┐ ┌───────────────┐ ┌──────────────────┐  │
│  │ Collectors   │ │ Data engine │ │ Rules engine │ │ Diagnostics   │ │ Privileged broker│  │
│  │ (ETW, WMI,   │→│ (TS store + │→│ (triggers →  │ │ (baselines,   │ │ (policy, audit,  │  │
│  │ SCM, sensors)│ │  SQLite)    │ │  actions)    │ │  correlation) │ │  rollback)       │  │
│  └──────────────┘ └─────────────┘ └──────────────┘ └───────────────┘ └──────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────────────────── ┘
                │
        ┌───────┴────────┐
        │ Atlas Updater  │  (scheduled task; verifies signed manifest; applies MSI)
        └────────────────┘
```

Key isolation properties (PRD §13.2):

* The **service** owns collection, storage, rules, diagnostics, and privileged actions. It has no UI dependencies and survives UI crashes.
* The **UI** is a stateless viewer over the service API. Killing it loses nothing.
* The **MCP server** (`atlas-mcp`) is a separate opt-in process with a read-only capability scope (no `AtlasControl`); it hosts no model, so there is no local inference to crash, and a hung MCP session cannot block collection or the UI thread. It is absent entirely unless the user enables it.
* The **emergency UI** depends on nothing but win32 + one pipe connection, is statically linked, and requests `HIGH_PRIORITY_CLASS`.
* The **updater** runs as its own scheduled task; the service never updates itself in place.

## 2.2 Why the service runs as LocalSystem

Kernel ETW sessions (process, disk, network providers), service control, other-user process inspection, and SRUM access all require administrative privileges. Running collection under LocalSystem (with unused privileges explicitly removed from its token at startup) and keeping **the UI unprivileged** is the least-privilege split that still satisfies the PRD: elevation is confined to one signed, audited binary (PRD §14.1). Per-action user consent is enforced at the API layer (see §5.3 of this document), not by UAC prompts per click.

---

# 3. Language Strategy

## 3.1 Decision: Rust core + C# WinUI 3 UI

**Rust for the service, engines, broker, emergency UI, MCP server, CLI.**

* Memory safety without garbage collection — the service must run for weeks with flat RSS and no GC-induced latency spikes; it is also the component parsing untrusted ETW payloads and serving a privileged API, exactly where memory-safety bugs are most costly.
* Excellent Windows story: the Microsoft-maintained `windows`/`windows-sys` crates expose the entire Win32/NT surface; `ferrisetw` (KrabsETW-inspired) handles ETW session management and event parsing; `tokio` provides named-pipe servers and async timers.
* Small static binaries (~2–8 MB), fast cold start, trivially meets the 100 MB service budget.

**C# / .NET 10 + WinUI 3 for the main UI.**

* WinUI 3 is the only framework that produces a *genuinely* Windows 11-native experience — Mica/acrylic materials, Fluent controls, dark mode, snap layouts, pen/touch, per-monitor DPI, and **UI Automation accessibility built in** (PRD §11.5 requires screen readers, high contrast, reduced motion — reimplementing UIA in a non-native toolkit is a project in itself).
* C#/XAML developer velocity for ~60 screens of product surface is far higher than C++ or Rust UI toolkits.
* .NET 10 with NativeAOT publishing (supported for WinUI 3 since Windows App SDK 1.6) reaches sub-500 ms cold starts and cuts steady-state memory substantially.
* The UI holds no data hostage: all state lives in the service, so a GC pause in the viewer can never lose samples.

## 3.2 Alternatives considered

| Option | Verdict | Reasoning |
|---|---|---|
| **All C#** (.NET service via NativeAOT + WinUI 3) | Strong runner-up | One language, one toolchain, `TraceEvent` library for ETW is excellent. Costs: higher baseline memory in the always-on service, GC tuning burden, larger attack surface in the privileged component. Choose this if the team is C#-only; the architecture is unchanged. |
| **All Rust** (service + Tauri/WebView2 or Slint UI) | Rejected for main UI | WebView2 UI cannot match Fluent fidelity, UIA accessibility, and dense 60 fps chart virtualization within the 200 MB budget without heroic effort; Slint/egui look non-native. Rust stays the right choice below the UI line. |
| **All C++** (service + WinUI 3/C++ or Direct2D custom UI) | Rejected | Maximum API familiarity, but slower delivery, and memory-unsafety in the privileged, network-of-parsers service is a real product risk (see WinRing0-class CVEs in this tool category). |
| **Electron / web shell** | Rejected outright | Violates §12 budgets (memory, GPU-while-minimized, battery) before the first feature ships. |
| **Qt/QML** | Rejected | Licensing cost or LGPL constraints, non-native look, weaker accessibility on Windows, still needs all the same native collectors. |

## 3.3 Managing a two-language codebase

* **Single contract source of truth:** all IPC messages and entities defined in `.proto` files; `prost`/`tonic` generate Rust, `Grpc.Tools` generates C#. No hand-written duplicate DTOs.
* **Buf** for proto linting and breaking-change detection in CI.
* The UI never touches Windows collection APIs directly (one narrow exception: the emergency UI's fallback process list). All knowledge of "how Windows works" lives in Rust.

---

# 4. Component Deep Dives

## 4.1 Collection Service (Rust)

**Runtime shape.** One process, three thread pools:

1. **ETW consumer threads** — one per active trace session (kernel session + user-manifest session), parsing events into a lock-free ring.
2. **Sampler thread** — periodic snapshot work that ETW cannot provide (see table), on an adaptive cadence.
3. **tokio async pool** — IPC server, storage flushes, WMI/COM calls (STA-confined where COM requires it), rule evaluation ticks.

**Collector-to-API map.** This table is the heart of the technical plan — each PRD data requirement mapped to its Windows source:

| Data (PRD ref) | Source API / provider | Notes |
|---|---|---|
| Process create/exit, image loads (§9.3.3) | ETW `Microsoft-Windows-Kernel-Process` | Event-driven; exact timestamps; no polling |
| Per-process CPU, memory, handles, threads (§9.2.2) | `NtQuerySystemInformation(SystemProcessInformation)` snapshot at 1 s | One syscall returns all processes; ~sub-ms; scales far better than per-PID queries |
| Context switches, DPC/ISR (§9.6.1) | Kernel ETW (CSWITCH/DPC flags) **only during incident recording** | Too hot to run continuously; enable on demand (§9.3.2 high-detail windows) |
| Per-process disk I/O (§9.6.3) | ETW `Microsoft-Windows-Kernel-Disk` / kernel DISK_IO flags + `IO_COUNTERS` deltas | Attribute to file when FILE_IO enabled during incidents |
| Disk latency, queue depth (§9.6.3) | ETW disk events (service time) + PDH `PhysicalDisk` counters | |
| Storage health/SMART, wear (§9.6.3) | `IOCTL_STORAGE_PROTOCOL_COMMAND` (NVMe log pages), `IOCTL_ATA_PASS_THROUGH` (SATA), fallback WMI `MSStorageDriver_*` | Admin required — we have it in the service |
| Per-process network bytes (§9.6.5) | ETW `Microsoft-Windows-Kernel-Network` (TCP/UDP send/recv per PID) | The proven per-process attribution path |
| Connection table, listening ports (§9.12) | `GetExtendedTcpTable` / `GetExtendedUdpTable` polled + reconciled with ETW flow events | |
| DNS resolution history (§9.12.1) | ETW `Microsoft-Windows-DNS-Client` | Maps IPs → domains without packet capture |
| Wi-Fi signal, adapters, VPN, metered (§9.6.5) | `wlanapi`, `GetAdaptersAddresses`, `INetworkCostManager` | |
| GPU per-process engine/VRAM (§9.6.4) | `D3DKMTQueryStatistics` + PDH "GPU Engine"/"GPU Process Memory" counters | Same source Task Manager uses; works unprivileged |
| GPU temps/clocks/power/fan (§9.6.4) | **NVML** (NVIDIA), **ADLX** (AMD), **IGCL** (Intel) vendor libraries, dynamically loaded | Cover the three vendors without a kernel driver |
| CPU frequency, throttling (§9.6.1) | `CallNtPowerInformation(ProcessorInformation)`, PDH `Processor Information` (% Performance Limit), `PowerRegisterSuspendResumeNotification` | Package temp/power via vendor/OEM paths where available; **honest capability labeling when absent** (PRD §9.6.7) |
| CPU package temperature (§9.6.7) | ACPI `MSAcpi_ThermalZoneTemperature` (often coarse) + OEM WMI (Dell/Lenovo/ASUS ACPI-WMI) where present | Full MSR/EC/SuperIO coverage deferred to the v2 driver decision (§4.9) |
| Memory composition: standby/modified/compressed, commit (§9.6.2) | `NtQuerySystemInformation(SystemMemoryListInformation)`, `GetPerformanceInfo`, `Microsoft-Windows-Kernel-Memory` ETW for pressure events | Enables the "high memory ≠ problem" explanation (§9.6.2) |
| Hard faults / paging (§9.3.3) | Kernel ETW MEMORY flags (sampled) + PDH `Memory\Pages Input/sec` | |
| Battery: rate, capacity, cycles (§9.6.6) | `IOCTL_BATTERY_QUERY_INFORMATION`/`QUERY_STATUS`, `GetSystemPowerStatus` | Design vs full-charge capacity for health |
| Per-app energy attribution (§9.6.6) | **SRUM** (`C:\Windows\System32\sru\SRUDB.dat`, ESE) best-effort + our own CPU/GPU/disk/network model | SRUM is undocumented-but-stable forensic gold (hourly per-app CPU/network/energy, even pre-install); read-only, defensive parsing, feature-flagged |
| Sleep/wake, wake sources (§9.3.3) | `Microsoft-Windows-Kernel-Power` ETW + `powercfg /lastwake` equivalent (`CallNtPowerInformation`) | |
| Camera/mic/location usage (§9.10) | **CapabilityAccessManager ConsentStore** registry (`...\ConsentStore\{webcam,microphone,location}\...\LastUsedTimeStart/Stop`, HKLM+HKCU, Packaged+NonPackaged) watched via `RegNotifyChangeKeyValue` + **live audio capture sessions** via `IAudioSessionManager2` on capture endpoints | The ConsentStore is what Settings itself displays; stable since Win10 1903 — still validated per-build in the compat suite |
| Screen capture detection (§9.10.1) | Graphics Capture API session events where exposed; `SetWinEventHook` heuristics | Labeled "where detectable" per PRD |
| Services: state, config, failures (§9.9.1) | SCM: `EnumServicesStatusEx`, `QueryServiceConfig(2)`, `NotifyServiceStatusChange`; `Microsoft-Windows-Services` ETW; System event log 7000–7045 | Event-driven state changes, no polling |
| Scheduled tasks (§9.9.2) | Task Scheduler 2.0 COM (`ITaskService`) + `Microsoft-Windows-TaskScheduler/Operational` log | |
| Startup inventory (§9.8.1) | Run/RunOnce keys (HKLM+HKCU, WOW64 views), Startup folders, `StartupApproved` state, StartupTask (packaged apps via `PackageManager`), services, tasks, `IShellLink` resolution | Autoruns-breadth is R2+; MVP covers the top sources |
| Boot phase timing (§9.8.4) | `Microsoft-Windows-Diagnostics-Performance/Operational` event 100 series + our service's own boot markers | Full ETW autologger boot traces = R2 |
| App inventory (§9.11) | Registry Uninstall keys (never `Win32_Product`), `PackageManager` for MSIX/Store, install source heuristics | |
| System changes (§9.13) | Windows Update Agent COM (`IUpdateSearcher::QueryHistory`), CBS/Setup event logs, driver events (`Microsoft-Windows-Kernel-PnP`), firewall change auditing, snapshot-diff of startup/services/tasks inventories | Diffing our own inventories catches changes with no event trail |
| Crashes, hangs, BSODs (§9.14) | WER event log (1000/1001/1002), LiveKernelReports/minidump metadata, `Win32_ReliabilityRecords` WMI, our own hang detector (`IsHungAppWindow` + input-idle probes) | |
| Frame times for game sessions (§7.2) | ETW `Microsoft-Windows-DxgKrnl` Present events — same data **Intel PresentMon** consumes (open source; embed the library or reimplement the consumer) | No overlay, no injection |
| Windows/session/user info (§9.19, §9.20) | `EnumWindows` + `GetWindowThreadProcessId` + DWM cloaking attributes; WTS APIs (`WTSEnumerateSessionsEx`) | |
| Efficiency mode state (§9.21) | `GetProcessInformation(ProcessPowerThrottling)` + priority + QoS ETW where available | |
| Handles/modules/threads/tokens on demand (§9.4) | `NtQuerySystemInformation(SystemHandleInformation)` + `NtQueryObject` (in a killable worker thread — some pipe handles block), `EnumProcessModulesEx`, thread enumeration + `NtQueryInformationThread`, `OpenProcessToken` + `GetTokenInformation` | On-demand inspector queries, never continuous |
| File-lock lookup (§9.5) | **Restart Manager** (`RmStartSession`/`RmGetList`) as the safe documented path; handle-table search as expert fallback; `MoveFileEx(MOVEFILE_DELAY_UNTIL_REBOOT)` for schedule-delete | Restart Manager works unprivileged for most cases |
| Signature/certificate verification (§9.4.1) | `WinVerifyTrust` + `WTHelperProvDataFromStateData` chain details; Authenticode hash cache keyed by (path, size, mtime) | Verify off the hot path; cache aggressively |

**Adaptive sampling (PRD §13.4).** The sampler runs a control loop: 1 s cadence while any process's CPU delta > threshold or UI is open; decays to 5 s, then 15 s at idle; instantly returns to 1 s on ETW activity bursts (process starts, I/O storms) or UI focus; user-initiated incident recording (§9.3.6) switches selected collectors (CSWITCH, FILE_IO) on for a bounded window at higher resolution. Every collector reports its own cost into the store (`atlas.self.*` metrics) — the product must display its own overhead (PRD §12.2, §21.2).

**Backpressure rule.** Ring buffers between ETW parsing and storage are bounded; under overload the service degrades by widening sampling intervals and dropping *derived* (not event) data first, and always records a `data_gap` marker so charts can render "missing data" honestly (PRD §11.3).

## 4.2 Local Data Engine (Rust)

**Two stores, one query façade:**

1. **SQLite (via `rusqlite`), WAL mode** — entities and events: processes (identity rows), applications, services, tasks, startup entries, privacy events, system changes, crashes, incidents, rules, rule executions, actions, experiments, audit log, settings. SQLite gives us transactional integrity, rich ad-hoc query for timeline event lanes, FTS5 for global search (§9.2.4), and painless export.
2. **Custom time-series store (`atlas-tsdb`)** — numeric samples: per-process CPU/RSS/IO/GPU, system counters, temperatures, battery.
   * **Layout:** append-only 2 MB chunk files per tier; within a chunk, series are columnar blocks.
   * **Encoding:** delta-of-delta timestamps + Gorilla XOR float compression (the Facebook/Prometheus scheme) — typically 1.5–3 bytes per sample vs 16 raw.
   * **Tiers (PRD §13.5):** T0 raw 1 s × 72 h (default) → T1 10 s roll-up (min/max/avg/p95) × 14 d → T2 1 min × 30–90 d. Peaks are preserved by storing max explicitly (PRD: "downsample while preserving significant events and peaks"). Bookmarked incident windows are pinned and never downsampled.
   * **Writes:** in-memory head blocks flushed every 30–60 s or 4 MB, whichever first — batched, sequential, SSD-friendly (PRD §12.4). Crash safety = lose at most the last flush interval of *samples* (events go through SQLite WAL immediately).
   * **Reads:** memory-mapped chunks, zone-map (min/max time per block) skipping, LTTB/M4 decimation server-side so the UI never receives more points than pixels.
   * **Compaction & retention:** idle-time job merges chunks, applies tier demotion, enforces the user's size cap; DB size surfaced in Settings (PRD §12.4).
   * **Cardinality control:** series identity = (metric, scope hash); short-lived PIDs roll up into their application identity after exit to avoid unbounded series growth.
3. **Query façade:** one Rust crate exposing typed queries (range scans, top-N-over-window, event correlation joins) consumed by the gRPC layer; the UI and CLI never see storage details, which lets us swap TSDB internals later without breaking clients. **DuckDB** is deliberately deferred to R3 as an optional analytical sidecar over exported Parquet for the "compare two weeks of sessions" class of query — not in the hot path.
4. **Redaction pipeline (§9.18, §15):** a single `Redactor` component applied at every *egress boundary* — report export and, critically, the MCP tool surface (usernames, hostnames, paths, IPs, domains, window titles, command lines, application names → stable pseudonyms) — never at collection time, so local views stay complete. Redaction defaults ON and stricter for MCP, since those results leave the machine for the client's model provider.

**Why not alternatives:** server TSDBs (Influx, Timescale, QuestDB) violate local-first/footprint; raw SQLite rows per 1 s sample would write ~GBs/day and shred the write-amplification budget; Parquet alone is immutable and awkward for a rolling 1 s head. The Gorilla-tier design is well-trodden (Prometheus, Netdata) and small enough to own (~3–5k LoC).

## 4.3 Rules Engine (Rust)

* **Representation:** rules are data (versioned JSON documents in SQLite), never code: `{triggers[], conditions[], actions[], scope, schedule, precedence, expiry}`. Triggers subscribe to the same internal event bus the collectors feed (process started, focus changed, fullscreen entered, AC/DC changed, thermal threshold, time window, session change) — the full PRD §9.7.1 list maps 1:1 to bus topics.
* **Actions** execute exclusively through the privileged broker (§4.5): priority (`SetPriorityClass`), I/O priority (`NtSetInformationProcess(IoPriorityHint)`), memory priority, affinity/CPU sets (`SetProcessDefaultCpuSets` — preferred over hard affinity for P/E-core steering), EcoQoS (`SetProcessInformation(ProcessPowerThrottling)`), power overlay (`PowerSetActiveOverlayScheme` — lightly documented, wrapped + feature-flagged), suspend/resume (`NtSuspendProcess` with safety checks), service start/stop, script hook (opt-in, signed-scripts-only option for fleets).
* **Determinism & conflicts (§9.7.6):** effective policy computed by a pure function `resolve(rules, state) → policy` with explicit precedence (profile > user rule > adaptive protection > default); the UI's conflict view and simulation mode (§9.7.5) call the same function with hypothetical state — simulation is *the same code path*, so previews can't lie.
* **Every execution** writes a rule-execution record with before-state → enables one-click rollback and the "did it help?" experiment loop (§9.15.3).
* **Dynamic responsiveness protection (§9.7.3, R3):** a watchdog on the bus (sustained CPU monopoly + foreground input latency probe) applies *temporary* EcoQoS/priority dampening with automatic restoration and a visible intervention record. Never touches processes on the protected-critical list.

## 4.4 Diagnostics Engine (Rust)

* **Baselines:** per-metric rolling statistics (EWMA + t-digest quantile sketches, persisted) give "is this normal *for this machine and this app*" — the anchor for abnormality sorting (§9.2.3) and incident detection (§9.3.7).
* **Detection:** threshold + duration detectors (cheap, always on) feed a changepoint pass (CUSUM/PELT-style) that trims incident boundaries precisely.
* **Correlation:** when an incident window exists, the engine assembles an **evidence graph**: entities (processes, services, changes, thermal events) linked to the window by temporal overlap, resource attribution share, causal hints (parent-child, service dependency, update-then-regression), and novelty (first-seen executable, new startup entry). Each edge carries a typed weight.
* **Confidence scoring (§9.15.2):** scores map to the PRD's fixed ladder (confirmed / high / medium / low / insufficient) via explicit rules — e.g. "attribution ≥ 70% of the saturated resource during ≥ 80% of the window" ⇒ high; mere temporal overlap caps at low. Alternative explanations are emitted whenever the top-two scores are within a margin. The wording engine renders hedged language exactly as PRD §3.2 mandates — this is a template system over the evidence graph, **not** free LLM text.
* **Playbooks:** each PRD §9.15.1 question is a typed playbook (slow-system, stutter, battery-drain, hot-fans, boot-regression, locked-file, wake-source, crash-loop…) declaring required evidence, queries, scoring, and recommended actions with reversibility metadata. Playbooks are data + small Rust functions; R3's plugin surface can add more.
* **Experiments (§9.15.3):** before/after windows compared with Mann-Whitney U + effect size on the target metric; underpowered data reports "insufficient evidence" rather than success (PRD §21.5).

## 4.5 Privileged Broker (in-service module)

* **Single chokepoint:** every mutation (kill, suspend, service change, startup toggle, firewall rule, priority/affinity, handle close) is a typed gRPC command handled by one module with: policy check (critical-process/service protection lists, §9.22), risk classification, required-consent level, execution, before/after capture, audit append.
* **Caller authentication:** named pipe SDDL restricts connections to SYSTEM + the interactive user's SID; per-connection the service verifies the client PID's session and executable signature (defense-in-depth; the real boundary is the pipe ACL — a local admin can bypass anything, which is out of scope of this threat model).
* **Consent tokens:** destructive commands require a `consent_token` minted only after the UI has displayed the §9.22 safe-end-task flow (unsaved-work risk via `GetGuiResources`/document-window heuristics, child processes, dependent services, restart prediction). The emergency UI mints its own scoped tokens.
* **Reversibility ledger (§3.3):** every reversible action pushes an undo record (old startup state, old service start type, old priority…) onto a persisted stack; system-level changes (service disable, driver-adjacent) additionally offer a **restore point** via the `SystemRestore` WMI class before proceeding.
* **Audit log (§14.5):** append-only SQLite table with hash chaining (each row carries SHA-256 of previous row) for tamper evidence; includes user actions, rule actions, exports, MCP enablement + per-tool calls with the returned payload (or hash + field summary), rollbacks, updates.
* **Handle force-close** (§9.4.3) is implemented via `DuplicateHandle(DUPLICATE_CLOSE_SOURCE)` from the service — gated behind expert mode + strongest warning tier, per PRD.

## 4.6 Main UI (C# / WinUI 3)

* **Framework:** Windows App SDK (latest stable, 1.7+), WinUI 3, C# on .NET 10, `CommunityToolkit.Mvvm` (source-generated MVVM), `CommunityToolkit.WinUI` controls where they fit.
* **Publishing:** NativeAOT (supported for WinUI 3 apps since WinAppSDK 1.6) → cold start well under the 1.5 s budget, ~40–60% lower steady memory than JIT; CI enforces AOT-compatibility (no reflection-heavy libs; `x:Bind` compiled bindings only).
* **Charts — the make-or-break UI component (§11.3):** no off-the-shelf XAML chart library survives 30+ synchronized dense tracks. We build `Atlas.Charts` on **Win2D** (`Microsoft.Graphics.Win2D`, Direct2D): retained damage-based redraw (only the advancing 1 s slice repaints during live view), server-side LTTB/M4 decimation to ≤ 2× pixel width, min/max envelope rendering so 50 ms spikes stay visible (PRD: "show peaks without hiding short events"), shared time-cursor across tracks, GPU-cheap when minimized (rendering suspended on `Window.VisibilityChanged`, PRD §12.5). Accessible fallback: every chart exposes a UIA-readable summary table (PRD §11.5).
* **Big lists:** `ItemsRepeater`-based virtualized tree/table for 300–3,000 processes with stable-sort throttling (re-sort at most 1×/s, animate row movement subtly) and column virtualization for the §9.2.2 expert column set.
* **Live data path:** UI reads the shared-memory ring directly (see §5.1) on a `DispatcherQueueTimer` aligned to its own refresh rate (1 s default, 2–5 s on battery per §12.6) — the service is never blocked by a slow UI.
* **Design system:** Fluent tokens, Mica window, Segoe Fluent Icons, semantic color roles defined once (normal/info/attention/warning/critical/privacy/system/suspended per §11.2) with non-color redundancy (icons/patterns) enforced by a lint pass over XAML resources.
* **Accessibility & i18n:** UIA names on all custom controls, high-contrast theme validation in CI screenshots, reduced-motion honors `UISettings.AnimationsEnabled`, `.resw` localization with ICU plural rules.
* **Tray helper:** tiny always-running WinUI-less C# (or Rust) tray process: global incident hotkey (`RegisterHotKey`, works unfocused per §9.3.6), toast notifications via Windows App SDK `AppNotificationManager`, quick health flyout. Kept separate so the heavy UI process can stay closed.

**Emergency UI (Rust, §12.7):** single statically-linked exe (<3 MB), raw Win32 window + owner-drawn list, `HIGH_PRIORITY_CLASS`, working set pre-touched at spawn; talks to the service pipe, falls back to direct `NtQuerySystemInformation` + `TerminateProcess` if the service itself is starved; launched via tray hotkey (Ctrl+Shift+Esc-style chord) or when the main UI misses its own watchdog heartbeat during launch.

## 4.7 Natural-Language Analysis: local deterministic + read-only MCP

**Direction (decision, 2026-07-13):** Atlas does **not** host an AI model or generate conversational answers. It is the *evidence provider*; the *model and the conversation* belong to the user's own MCP-compatible client (Claude, ChatGPT, or any MCP host). This deletes the entire local-model stack (ONNX Runtime GenAI, DirectML/NPU paths, llama.cpp, Phi/GGUF downloads, Ollama/LM Studio detection, remote-endpoint config, grammar-constrained decoding, the `atlas-ai` host process, hallucination test harnesses) in exchange for a thin read-only adapter over the query API that already exists (`AtlasQuery`). Net: far less implementation, packaging, and privacy surface; better alignment with the product's "trusted evidence, not unsupported claims" thesis.

Two tiers remain, neither hosts a model:

**Tier 1 — Deterministic local analysis (in-app, always available, no model, PRD §9.16.2 "No-AI mode").** The Diagnostics "ask a question" box runs template/intent matching (keyword grammar → parameterized playbook queries) entirely offline. This is the in-app natural-language affordance; it produces the same structured, cited output the diagnostics engine already emits. Non-MCP users lose nothing in-app.

**Tier 2 — Read-only MCP server (`atlas-mcp`, opt-in, R2).** A separate small Rust process the user registers in their MCP client. It speaks MCP (JSON-RPC 2.0, stdio) to the client and translates each tool call into **read-only** `AtlasQuery` RPCs over the existing named pipe. It hosts no model; the client's model does the reasoning and writes the answer.

* **Tools** (map ~1:1 onto existing RPCs): `query_timeline`, `top_consumers`, `find_events`, `diff_periods`, `explain_process`, `get_incident`, `get_playbook_result`, `list_system_changes`, `find_crashes`.
* **Every result is self-describing** — evidence IDs, time range, process identities, relevant metrics/events, confidence level, missing-data markers (`data_gap`/`outside_retention`/`sensor_unavailable`), and retention/sensor limitations as first-class fields, plus a machine-readable `grounding` block with a suggested citation string. This preserves the grounding design; the client renders it.
* **Isolation (PRD §13.2):** `atlas-mcp` is its own process. It connects with a **read-only capability scope that excludes `AtlasControl`** — a tool call can never suspend/kill a process or change a rule. A crash or hang affects only the MCP session, never collection. Any action a model suggests becomes a button in the Atlas UI routed through the normal human-consent + broker path — never auto-executed.
* **Prompt-injection posture:** process names, window titles, and command lines are untrusted input carried as data in structured fields, never as instructions; and because the MCP surface is read-only, an injected "kill this process" can't act.

**The honest limitation (§9.16.1, must be documented in the PRD):** Atlas guarantees its MCP tools return *grounded, citation-ready* data. It **cannot** guarantee the external model's final answer contains no unsupported claims — Atlas controls the tool results, the client controls the conversation and the response. The old "removes uncited claims before displaying" promise was only possible with a built-in assistant; under MCP it becomes "provides citation-ready evidence."

**Privacy — the boundary got more important, not less (§9.16.3, §15).** With no local model, in-app analysis never leaves the machine. But an MCP tool result **egresses to the client's model provider** (e.g. Anthropic/OpenAI servers) the moment the client reads it. Therefore, for the MCP surface specifically: disabled by default; explicit user enablement; **redaction default-ON and stricter than local views** (paths, usernames, domains, window titles, command lines, application names — all configurable); read-only tools only; per-tool result-size and time-range caps; a clear warning that returned data leaves Atlas's security boundary; instant revoke. Atlas cannot preview the client's full prompt, but it **audits its own side of the boundary**: every tool call and the exact payload it returned (or a hash + field summary) is logged via the shared `Redactor` + audit table (§14.5).

## 4.8 Shell & OS Integration

* **File Explorer "Find what is using this file" (§9.5, §17.3):** `IExplorerCommand` COM handler registered via a **sparse MSIX package** (required for the Windows 11 modern context menu; classic registry verb as Win10-style fallback). The handler is a thin forwarder: it passes the path to the running UI/service and exits — no logic in Explorer's process.
* **Toasts:** Windows App SDK `AppNotification` with actionable buttons (deep-link into incident/timeline).
* **Startup:** service auto-start (delayed); tray helper via Run key (or `StartupTask` with package identity); UI on demand.
* **CLI (R3):** `atlas` (Rust, `clap`): `atlas top`, `atlas ports`, `atlas locks <path>`, `atlas incident list`, `atlas export --redact` — same gRPC surface, JSON/CSV output; thin PowerShell module (`Get-AtlasProcess`…) wrapping it for admin scripting (§7.5).

## 4.9 Kernel Driver Policy (PRD §13.6)

**v1 ships with no kernel driver.** Everything in §4.1's table is user-mode. Consequences, stated honestly in-product: CPU package temperature and fan RPM coverage limited to what ACPI/OEM-WMI/vendor GPU libraries expose — the UI's sensor page labels unavailable sensors as such (PRD §9.6.7 requires this anyway).

**v2 decision gate** (run the PRD checklist with real data): if telemetry shows sensor coverage is a top user gap, evaluate in order: (a) integrating an established **signed, maintained** sensor-access driver with a sandboxed module model (e.g., PawnIO — verify licensing and signing posture at decision time); (b) our own minimal KMDF **read-only** sensor driver (MSR/EC/SuperIO read paths only, no arbitrary physical-memory mapping — the WinRing0 lesson), attestation-signed, HVCI-compatible, with its own update channel. Either way the product keeps a full no-driver mode (PRD §13.6).

---

# 5. IPC Design

| Channel | Transport | Encoding | Direction | Purpose |
|---|---|---|---|---|
| Live metrics | Shared memory section (`CreateFileMapping`) + seqlock versioning | Fixed-layout structs (repr(C)) | Service → readers | 1 Hz snapshot of top-N process rows + system gauges; UI/tray/emergency read lock-free at their own cadence |
| Queries | gRPC over **named pipes** | proto3 | Client ↔ service | Timeline ranges, inspector detail, search, reports |
| Event push | gRPC server-streaming | proto3 | Service → UI | Incidents, alerts, rule activations, privacy events (drives toasts + live event lanes) |
| Commands | gRPC unary + consent tokens | proto3 | UI/CLI → broker | All mutations (§4.5) |
| MCP (opt-in) | JSON-RPC 2.0 over stdio (client ↔ `atlas-mcp`); read-only gRPC over the pipe (`atlas-mcp` ↔ service) | MCP / proto3 | User's MCP client → `atlas-mcp` → service | Grounded read-only query tools; no `AtlasControl` in scope |

Implementation notes:

* **Named pipes both sides:** Rust `tonic` served over `tokio::net::windows::named_pipe` (hyper accepts any AsyncRead/Write connection stream); C# clients connect gRPC through `SocketsHttpHandler.ConnectCallback` → `NamedPipeClientStream`. (Kestrel has native named-pipe support since .NET 8, useful if any C# component ever serves.) No TCP ports are opened — important for a tool that itself audits listening ports.
* **Pipe security:** SDDL grants connect to SYSTEM + `INTERACTIVE` (further narrowed to the console session's user SID at runtime); message-mode pipes; per-message size caps; slow-consumer disconnect (the service never blocks on a reader).
* **Versioning:** Buf-managed protos, additive-only within a major; the service advertises capability flags (e.g., `has_gpu_vendor_telemetry`, `has_driver_sensors`) so older UIs degrade gracefully — this is also how reduced-capability mode (no driver, missing sensors) propagates to UX.

---

# 6. Data Model → Storage Mapping (PRD §13.3)

| PRD entity | Home | Notes |
|---|---|---|
| Device, user session | SQLite `device`, `session` | Session rows anchor per-user separation (§14.4) |
| Application | SQLite `application` | Identity = publisher + product heuristics; groups processes (§9.2.1) |
| Process | SQLite `process_instance` (one row per PID lifetime) | ETW create/exit stamped; FK to application; command line/paths stored here |
| Thread, module, handle detail | Not persisted continuously | On-demand inspector snapshots; incident recordings may pin a snapshot blob |
| Service, scheduled task, startup entry | SQLite inventories + change-diff triggers | Diffs feed `system_change` |
| Resource sample | `atlas-tsdb` tiers | Series key = (metric, scope) |
| Event | SQLite `event` (typed, indexed by time + entity) | Timeline lanes query this + FTS5 |
| Incident, recommendation, action, experiment | SQLite, linked to evidence via `evidence_edge` | The §9.15.2 output structure is a view over these tables |
| Rule, rule execution | SQLite `rule`, `rule_execution` | Execution rows carry before-state for rollback |
| Privacy capability event | SQLite `privacy_event` | Start/stop/duration/foreground/locked flags (§9.10.2) |
| System change | SQLite `system_change` | Before/after JSON, reversal pointer (§9.13) |
| Audit record | SQLite `audit` (hash-chained) | §14.5 |
| Report | Generated artifact + SQLite metadata | HTML template → PDF via WebView2 print pipeline; CSV/JSON direct from query façade |

Example core DDL sketch (illustrative):

```sql
CREATE TABLE process_instance (
  id INTEGER PRIMARY KEY,
  app_id INTEGER REFERENCES application(id),
  pid INTEGER NOT NULL, parent_id INTEGER,
  image_path TEXT, command_line TEXT, user_sid TEXT, session_id INTEGER,
  integrity TEXT, signature_status TEXT, sha256 TEXT,
  start_time_ns INTEGER NOT NULL, exit_time_ns INTEGER, exit_code INTEGER
);
CREATE INDEX ix_proc_time ON process_instance(start_time_ns, exit_time_ns);

CREATE TABLE event (
  id INTEGER PRIMARY KEY, ts_ns INTEGER NOT NULL,
  kind INTEGER NOT NULL,             -- enum: proc_start, svc_stop, crash, privacy, change...
  entity_kind INTEGER, entity_id INTEGER,
  severity INTEGER, payload JSONB
);
CREATE INDEX ix_event_time_kind ON event(ts_ns, kind);
```

---

# 7. Security Architecture (PRD §14)

* **Code signing:** every binary, the MSI, the sparse MSIX, and update manifests are Authenticode-signed (EV certificate or Azure Trusted Signing); the service refuses to load unsigned plugins (R3) and the updater refuses unsigned manifests/packages.
* **Update security (§14.3):** static-key-pinned signed JSON manifest (TUF-inspired: separate offline root key signs the manifest key), HTTPS + signature required, staged rings (canary → beta → stable) with health-gated promotion, one-click rollback to previous MSI kept locally, separate expedited security channel.
* **Hardening flags:** Rust builds with `/guard:cf` (Control Flow Guard), CETCOMPAT, high-entropy ASLR; C# UI with CET/ASLR-compatible host settings; service token strips unneeded privileges at start and applies a restricted DACL to itself.
* **Data protection (§14.4):** database directory ACL'd to SYSTEM + the owning user; optional at-rest encryption (SQLite + chunk files encrypted with a DPAPI-protected key); per-user data separation keyed by SID; secure-delete = key destruction when encryption is on; export always passes the consent + redaction sheet.
* **Supply chain:** `cargo-deny`/`cargo-audit` + fail-closed transitive NuGet audit run in CI; Cargo and NuGet resolution is lockfiled; a CycloneDX SBOM is attached automatically to future published GitHub releases. Dependency vendoring and reproducible-build work remain goals.
* **Threat-model note:** the product defends the *unprivileged→privileged* boundary (pipe ACLs, consent tokens, policy lists) and the *update* channel. It does not claim to defend against an already-admin attacker (consistent with every tool in this category).

---

# 8. Packaging, Distribution, Updates (target state)

* **Installer target:** WiX (v5/v6) per-machine MSI containing: service (+ recovery/restart config), UI (NativeAOT, per-arch), tray helper, emergency UI, sparse MSIX (shell extension identity), CLI (R3). x64 and **ARM64** are target architectures; the current RC is x64 only and carries an unpackaged, self-contained, non-NativeAOT WinUI payload.
* **Channels:** winget manifest, direct download, enterprise MSI with admin-template (ADMX) settings; Microsoft Store deferred (service + sparse-package composition is awkward under Store packaging today).
* **Updates:** the updater scheduled task checks the signed manifest daily/idle, downloads delta or full MSI, applies with service-drain (flush + stop → upgrade → start); UI prompts, never force-restarts a session (PRD "no surprise disruptions" ethos); release notes shown pre-apply (§14.3).
* **Crash reporting:** WER LocalDumps registered for all our processes + in-process minidump writer; **opt-in** upload (Sentry Native or self-hosted equivalent) with the same redaction gate; symbol server retained per release.

---

# 9. Repository, Toolchain, CI/CD, Testing

## 9.1 Target monorepo layout

```
system-atlas/
├─ proto/                    # single source of truth for all IPC + entities (buf.yaml)
├─ crates/
│  ├─ atlas-service/         # service host, collectors, broker
│  ├─ atlas-collectors/      # one module per collector (etw, scm, sensors, privacy…)
│  ├─ atlas-tsdb/            # time-series store
│  ├─ atlas-store/           # sqlite layer, migrations (refinery)
│  ├─ atlas-rules/ atlas-diag/ atlas-redact/
│  ├─ atlas-mcp/ atlas-emergency-ui/ atlas-cli/   # atlas-mcp: read-only MCP server (R2)
│  └─ atlas-ipc/             # tonic named-pipe glue, shared-mem ring
├─ src-ui/
│  ├─ Atlas.App/             # WinUI 3 shell, views
│  ├─ Atlas.ViewModels/      # MVVM, testable without XAML
│  ├─ Atlas.Charts/          # Win2D renderer
│  └─ Atlas.IpcClient/       # generated gRPC client + ring reader
├─ shell-ext/                # IExplorerCommand (C++ or Rust COM) + sparse MSIX
├─ installer/                # WiX
├─ tools/                    # trace recorder/replayer, perf harness, fault injector
└─ docs/                     # this file, ADRs, playbook specs
```

Target build orchestration uses `just` (or Nuke) to drive cargo, dotnet, and WiX coherently. The current repository uses direct `cargo`, `dotnet`, and PowerShell commands; no `just` or Nuke entry point has landed.

## 9.2 Testing strategy (target, mapped to PRD §19 metrics)

| Layer | Approach |
|---|---|
| Collectors | **ETW record/replay harness**: real traces captured from lab machines replayed deterministically in CI — collector logic is tested without needing the kernel to misbehave on cue |
| Parsers (ETW payloads, SRUM, protocols) | `cargo-fuzz` fuzz targets — these consume untrusted/undocumented formats |
| TSDB | Property tests (roundtrip encode/decode, tier-demotion invariants: max never lost), crash-recovery tests with kill -9 at random flush points |
| Rules/diagnostics | Golden-file scenario tests: synthetic event streams → expected incidents/confidence levels; the §17 user flows encoded as acceptance scenarios |
| Broker | Policy unit tests + negative tests (unauthorized SID, stale consent token, critical-process protection) |
| UI | ViewModel unit tests (no XAML needed); **FlaUI** (UIA3) end-to-end smoke on the built app: launch, search, kill notepad, open timeline; high-contrast + 200% DPI screenshot diffs |
| Performance gates | CI perf rig (self-hosted bare-metal runner) asserts the §12 budgets every merge: idle CPU (5-min window), service RSS, UI cold start (ETW-measured), disk writes/hour, timeline query p95. **A budget regression fails the build** — this is how "low overhead" survives feature pressure |
| Soak | Nightly 72 h run with synthetic workload + leak detection (RSS slope, handle counts — the tool watches itself) |
| Compatibility | Hyper-V matrix: Win11 23H2 / 24H2 / 25H2 / current Insider, x64 + ARM64; ConsentStore/SRUM/undocumented-API canary tests run here first (PRD §21.3 mitigation) |
| Security | CodeQL, clippy pedantic, `cargo-deny`; periodic external review of the broker + updater before 1.0 (PRD §21.4) |

**Current automation:** GitHub-hosted runners execute Rust formatting, Clippy, and workspace tests; restore and build the x64 WinUI app from locked NuGet graphs; run IPC-client and source-level UI contract tests; and launch the real unpackaged app for a UI Automation shell/navigation smoke. Separate workflows enforce RustSec and `cargo-deny`, fail-closed transitive NuGet auditing, weekly dependency updates, and CycloneDX attachment when future GitHub releases are published. Rust+C# CodeQL and pull-request dependency review are configured but entitlement-gated while the repository is private and personal; they activate for a public repository or after moving to an organization with GitHub Code Security and setting `ENABLE_GITHUB_CODE_SECURITY=true`. `perf.yml` enforces hosted-runner working set and a short soak, with idle CPU advisory. FlaUI breadth, screenshot diffs, the 72-hour bare-metal soak, the compatibility matrix, signed release production, active CodeQL/dependency review, dependency vendoring/reproducible builds, retroactive SBOM coverage for the existing RC, and hardware-sensor jobs remain target work.

---

# 10. Performance Budget Engineering (PRD §12, target budgets)

| Budget | Design tactic | Verified by |
|---|---|---|
| Idle CPU < 0.2% avg | ETW-driven (no polling for events); 1 syscall/sec snapshot for gauges, decaying to 15 s at idle; WMI/COM strictly off the hot path; no timers < 1 s except during incident recording | CI perf gate + in-product self-meter |
| Service RSS < 100 MB | Rust (no GC heap); bounded rings; mmap'd cold chunks (page cache, not heap); string interning for paths/names; head blocks capped (~16 MB) | Soak test slope + gate |
| UI RSS < 200 MB | NativeAOT; virtualized lists; decimated chart data (≤ 2× pixels); bitmap caches evicted on minimize | Gate on cold + 30-min-use RSS |
| UI visible < 500 ms | Service pre-warmed (UI is a viewer); NativeAOT; shell renders skeleton from shared-mem snapshot **before** first gRPC roundtrip; charts hydrate lazily (§12.1: process list before historical charts) | ETW startup trace in CI |
| Disk: no per-sample writes | 30–60 s batched flushes; WAL; compaction idle-only; target < ~150 MB written/day default config | Write-counter gate |
| GPU while minimized ≈ 0 | Rendering suspended on visibility loss; live refresh decoupled from vsync; low-rendering mode toggle (§12.5) | Manual + PresentMon check |
| Battery | Sampler widens to 5–15 s on DC; chart refresh drops; high-detail recording requires explicit action on battery (§12.6); own energy impact displayed from SRUM/self-model | Battery-rundown lab test |
| Responsive under 100% CPU | Emergency UI at HIGH_PRIORITY_CLASS; service control loop thread boosted; UI pipe requests carry deadlines and degrade to cached data; kill path touches only pre-resident code (§12.7) | Stress-suite scenario (CPU+memory+disk saturation, then: open UI, search, kill) |

---

# 11. Release Phasing (maps PRD §18)

**MVP (§18.1):** service + collectors for process/CPU/memory/disk/network/GPU basics, ConsentStore privacy events, SCM + startup basics; SQLite + T0/T1 TSDB (72 h); grouping heuristics; WinUI shell with Overview, Live Activity, Timeline (view/zoom/bookmark), process detail, safe end-task flow, search (FTS5); incident bookmarks + basic detectors; template-based diagnostic summaries (no LLM required to ship); HTML/PDF/CSV incident report with redaction; MSI + winget; perf gates live from week one.

**R2 (§18.2):** deep inspector (handles/modules/threads via on-demand snapshots), Restart-Manager file locks + Explorer sparse-MSIX integration, rules engine + profiles + simulation, boot analysis (event 100 + autologger), scheduled tasks, full network inspector (DNS ETW + per-process flows), battery/thermal analytics + vendor GPU libs, experiments, **read-only MCP server** (`atlas-mcp`: grounded query tools for the user's own MCP client — replaces the abandoned local-AI ladder; now one of the *cheaper* R2 items since the query API already exists), advanced privacy alerts.

**R3 (§18.3):** dynamic responsiveness protection, extended retention tiers + optional DuckDB/Parquet analytics, crash correlation depth, system-change tracking completeness, CLI + PowerShell module, out-of-proc signed plugin framework (gRPC surface, capability-scoped), support bundle, driver decision gate (§4.9).

---

# 12. Alternatives Considered and Rejected (summary ledger)

| Area | Rejected | Why |
|---|---|---|
| UI | Electron, Tauri/WebView2, Qt, Flutter, WPF | Budgets, native fidelity, accessibility economics (WPF: viable but yesterday's stack — weaker Fluent/touch/Mica story than WinUI 3) |
| Service language | C++ (unsafe surface), C# (GC/footprint in always-on privileged proc — acceptable fallback), Go (GC + cgo friction with COM/ETW) | Rust wins on safety-per-byte-of-overhead |
| Storage | InfluxDB/QuestDB/Timescale (servers), raw-SQLite samples (write amplification), Parquet-only (no rolling head), OSQuery (query model, not a recorder) | Embedded Gorilla-tier TSDB + SQLite is the fit |
| IPC | COM out-of-proc (works, but weak typing/versioning ergonomics), raw TCP localhost (opens ports — unacceptable for this product), WCF-era tech | gRPC/named-pipes + shared memory |
| Sensors | Ship WinRing0-style driver in v1 | Documented CVE history in this exact category; earn the driver, don't start with it |
| AI | Hosting any model in Atlas — local (ONNX Runtime GenAI / llama.cpp / Phi) or a built-in cloud-endpoint selector | Made Atlas own the *answer* (the hard, trust-eroding part) and bloated packaging with inference runtimes. Replaced by: deterministic local playbook matching (in-app) + a read-only MCP server that hands grounded evidence to the user's own client, which owns the model |

---

# 13. Validation Spikes (do these first)

Ordered, each ≤ 1 week, each de-risks a load-bearing assumption:

1. **tonic-over-named-pipes + C# `ConnectCallback` client** — round-trip latency, streaming throughput, ACL behavior across sessions.
2. **ETW cost harness** — kernel session with process+disk+network flags on a mid-range laptop: measure %CPU at idle and under load; tune buffer counts; validates the 0.2% budget's biggest line item.
3. **WinUI 3 NativeAOT + Win2D** — confirm AOT compatibility of the exact package set; measure cold start and 30-track chart redraw cost.
4. **TSDB slice** — Gorilla encode/decode + tiering for 300 processes × 6 metrics × 72 h; verify size (~target < 1 GB default) and range-query p95 < 50 ms.
5. **ConsentStore + SRUM probes** across 23H2/24H2/25H2 — stability of the undocumented surfaces we lean on; wire canary tests.
6. **D3DKMT/GPU counters on ARM64 + hybrid iGPU/dGPU** — attribution correctness where Task Manager itself struggles.
7. **PresentMon library embedding** — license review + frame-time capture overhead during a real game session.
8. **MCP server spike (R2)** — stand up `atlas-mcp` (JSON-RPC/stdio) exposing 2–3 read-only tools over the existing `AtlasQuery` pipe; register it in a real MCP client (Claude Desktop) and confirm grounded results render with confidence + missing-data markers, that redaction is applied to tool output, and that no `AtlasControl` surface is reachable.

---

# Appendix A — Key Third-Party Components

| Component | Use | License (verify at adoption) |
|---|---|---|
| `windows` / `windows-sys` crates | Win32/NT bindings | MIT/Apache-2.0 |
| `ferrisetw` | ETW sessions/parsing | MIT/Apache-2.0 |
| `tokio`, `tonic`, `prost` | async, gRPC, proto | MIT/Apache-2.0 |
| `rusqlite` + SQLite | relational store | MIT / public domain |
| Windows App SDK / WinUI 3 | UI framework | MIT (SDK components) |
| `Microsoft.Graphics.Win2D` | chart rendering | MIT |
| `CommunityToolkit.*` | MVVM, controls | MIT |
| MCP Rust SDK (or hand-rolled JSON-RPC 2.0 over stdio) | `atlas-mcp` server transport (R2) | verify at adoption; no model runtime shipped |
| Intel PresentMon (library) | frame-time ETW consumption | MIT — re-verify |
| NVML / ADLX / IGCL | GPU vendor telemetry | vendor SDK terms — legal review |
| FlaUI | UI automation tests | MIT |
| WiX Toolset | installer | MS-RL — build-time only |
| Sentry Native (opt-in) | crash reporting | MIT |

# Appendix B — Decision Records to Maintain

Adopt lightweight ADRs in `docs/adr/`; the decisions in this document seed ADR-001…015 (language split, LocalSystem service, no-driver-v1, TSDB-over-SQLite-samples, gRPC/named-pipes, WinUI 3, NativeAOT, consent-token broker, evidence-graph diagnostics, **read-only MCP integration instead of a hosted model** (2026-07-13), sparse-MSIX shell integration, WiX MSI, staged updates, perf-gates-in-CI, ARM64-day-one). Revisit each only with new evidence — especially the driver gate and the DuckDB deferral.
