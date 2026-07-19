# System Atlas

A Windows system intelligence and control application: one coherent replacement for Task Manager, Process Explorer, Process Lasso, and historical monitors — real-time monitoring, historical timelines, evidence-backed diagnostics, and safe reversible actions.

> Observe → Record → Detect → Explain → Recommend → Act → Verify → Reverse

**Status:** the first release candidate, **[v0.3.0-rc.1](https://github.com/iEssam/System-Atlas/releases/tag/v0.3.0-rc.1)**, is published for evaluation with a per-machine MSI. The planned MVP, R2, and R3 feature slices are substantially implemented, but the project is **not production-ready**: the milestone tracker still contains open and deferred work, the release artifact is unsigned, and the stable distribution/update path is not complete. See [docs/current-state.md](docs/current-state.md) for the as-built baseline and [docs/phases.md](docs/phases.md) for implementation status and release gates.

## What it does

- **Collection (user-mode only, no kernel driver — see [ADR-0001](docs/adr/0001-kernel-driver-decision-gate.md)):** ETW process/image events, `NtQuerySystemInformation`, SCM/services, registry & ConsentStore watchers, Restart Manager, GPU (D3DKMT + vendor libraries), battery, ACPI thermal via WMI, and per-process security metadata (Authenticode + cert chain, token privileges, mitigation policies).
- **Storage:** SQLite (WAL) for entities/events plus a custom Gorilla-compressed time-series store with tiered T0/T1/T2 roll-ups that preserve peaks and honor bookmark pins.
- **Intelligence:** threshold+duration incident detection (CPU saturation, memory pressure), evidence-based diagnosis with confidence-laddered contributing factors (no LLM), and a fully redacted support bundle.
- **Control:** a rules engine (priority / affinity / EcoQoS) with guaranteed reversibility, named rule profiles, and a dynamic responsiveness-protection watchdog that dampens a runaway process and auto-restores it.
- **Experiments:** save two evidence windows around a change and compare resource averages, peaks, threshold duration, process starts, crashes, and system changes with explicit data-quality and causation caveats.
- **Privacy:** live camera / microphone / location usage alerts sourced from the ConsentStore.
- **Forensics:** system-change tracking, crash correlation, and boot analysis.
- **Extensibility (read-only):** a [read-only MCP server](crates/atlas-mcp/README.md) exposing grounded query tools to your own MCP client, and a signed, capability-scoped plugin framework (plugins are Authenticode-verified, registered disabled until explicitly enabled, and every call is scope-checked; mutations are always denied).

## Architecture

A **LocalSystem collection service** (Rust) hosts the collectors, store, rules engine, and diagnostics, and brokers privileged actions. A **WinUI 3 desktop app** (C#/.NET 10) is the primary surface. They communicate over **gRPC on Windows named pipes** plus a lock-free **shared-memory ring** for the live fast path. The IPC contract in [proto/atlas.proto](proto/atlas.proto) (package `atlas.v0`) is the single source of truth.

## Repository layout

```
crates/
  atlas-collectors/     user-mode Windows collectors (ETW, NT query, SCM, sensors, security metadata)
  atlas-store/          SQLite-backed store (entities, events, incidents, plugins, rules)
  atlas-tsdb/           Gorilla-compressed time-series store with tiered roll-ups
  atlas-ipc/            gRPC/named-pipe + shared-memory-ring transport and generated contract
  atlas-service/        service host + dev-console CLI (top/record/serve/diagnose/plugin/...)
  atlas-mcp/            read-only MCP server exposing grounded query tools
  atlas-cli/            standalone command-line client
  atlas-plugin-example/ reference plugin proving capability-scope enforcement
src-ui/                 C#/.NET 10 WinUI 3 app, IPC client, and UI contract tests (Atlas.sln)
installer/              per-machine WiX v5 MSI (install / upgrade / removal + crash-restart)
proto/                  protobuf IPC contract (single source of truth)
scripts/                elevated validation, UI smoke, and documentation checks
docs/                   phase tracker, ADRs, release notes
```

## Development quickstart

Requires stable **Rust** (MSVC toolchain, `winget install Rustlang.Rustup`) for the core, and the **.NET 10 SDK** (`winget install Microsoft.DotNet.SDK.10`) to build the WinUI app under `src-ui/`.

```powershell
# live top-style view (1 s sampling)
cargo run -p atlas-service -- top

# record aggregated samples to %LOCALAPPDATA%\SystemAtlas\dev\atlas.db
cargo run -p atlas-service -- record --flush-secs 15

# detect + explain an incident from recorded data
cargo run -p atlas-service -- incidents --minutes 10
cargo run -p atlas-service -- diagnose --incident 1

# host the AtlasQuery contract over a named pipe for the UI/clients
cargo run -p atlas-service -- serve

# build the WinUI desktop app for the supported local architecture
dotnet build src-ui/Atlas.App/Atlas.App.csproj -c Debug -p:Platform=x64
```

Tests and lints (CI enforces both the Rust and Windows UI sequences):

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
dotnet test src-ui/Atlas.sln -c Debug -p:Platform=x64

# launch the real WinUI app and verify shell + Graphics navigation through UI Automation
./scripts/ui-smoke.ps1 -StartPage graphics -ExpectedElementName Graphics
```

### Dev data locations

Build artifacts stay in `target/` (gitignored). The dev database defaults to `%LOCALAPPDATA%\SystemAtlas\dev\atlas.db`, deliberately outside the repo.

## Installation (MSI)

Per-machine MSI (elevation required; registers and starts the `SystemAtlas` service):

```powershell
msiexec /i SystemAtlas-0.3.0.0-x64.msi
```

Supports clean install, in-place major upgrade, and clean removal (`msiexec /x`). The MSI carries the current unpackaged, self-contained WinUI x64 payload; sparse-MSIX shell integration is still deferred. The `%ProgramData%\SystemAtlas` data directory is preserved across uninstall so a reinstall keeps history. The RC's MSI is **unsigned** — sign it before distribution (see [installer/README.md](installer/README.md)). Build it yourself with `installer/build.ps1`.

## Validation

Runtime paths that need elevation, hardware, or a GUI are validated by two runbooks meant to run in an elevated, WDAC-exempt session:

- [scripts/elevated-validation.ps1](scripts/elevated-validation.ps1) — live ETW, service install/start/crash-restart/uninstall, incident detection + redacted bundle, and plugin capability enforcement (14 automated checks).
- [installer/validate-install.ps1](installer/validate-install.ps1) — the full MSI clean-install → upgrade → removal lifecycle.

Pull requests also build the x64 WinUI app, run both .NET test projects, and execute [scripts/ui-smoke.ps1](scripts/ui-smoke.ps1) against the real unpackaged app. This is a launch/navigation smoke check, not a substitute for the open high-DPI, High Contrast, long-run, and compatibility-matrix validation.

## Documents

| Doc | Purpose |
|---|---|
| [project.md](project.md) | Product Requirements Document (full product definition) |
| [tech-stack.md](tech-stack.md) | Technology stack & technical design |
| [docs/current-state.md](docs/current-state.md) | Current as-built and CI baseline |
| [docs/phases.md](docs/phases.md) | Implementation phases and milestone tracker |
| [docs/adr/](docs/adr/README.md) | Architecture Decision Records |
| [docs/releases/v0.3.0-rc.1.md](docs/releases/v0.3.0-rc.1.md) | Release notes |
| [proto/atlas.proto](proto/atlas.proto) | IPC contract (package `atlas.v0`) |
