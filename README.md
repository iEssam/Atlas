# System Atlas

A Windows system intelligence and control application: one coherent replacement for Task Manager, Process Explorer, Process Lasso, and historical monitors — real-time monitoring, historical timelines, evidence-backed diagnostics, and safe reversible actions.

> Observe → Record → Detect → Explain → Recommend → Act → Verify → Reverse

## Documents

| Doc | Purpose |
|---|---|
| [project.md](project.md) | Product Requirements Document (full product definition) |
| [tech-stack.md](tech-stack.md) | Technology stack & technical design |
| [docs/phases.md](docs/phases.md) | Implementation phases and milestone tracker |
| [proto/atlas.proto](proto/atlas.proto) | IPC contract sketch (compiled at milestone M4) |

## Repository layout

```
crates/
  atlas-collectors/   user-mode Windows collectors (process snapshots, gauges; ETW at M3)
  atlas-store/        SQLite-backed local store (events, entities, interim samples)
  atlas-tsdb/         time-series store (interim in-memory head; chunked Gorilla store at M-TSDB)
  atlas-service/      service host binary (console dev mode today; Windows service later)
proto/                protobuf contracts (single source of truth for IPC)
docs/                 phase tracker, ADRs
```

## Development quickstart

Requires stable Rust (MSVC toolchain) — `winget install Rustlang.Rustup`.

```powershell
# live top-style view (1 s sampling, one syscall per tick)
cargo run -p atlas-service -- top

# record aggregated samples to %LOCALAPPDATA%\SystemAtlas\dev\atlas.db
cargo run -p atlas-service -- record --flush-secs 15

# query what was recorded
cargo run -p atlas-service -- db-top --minutes 10

# one-shot JSON process snapshot
cargo run -p atlas-service -- snapshot
```

Tests and lints (CI enforces all three):

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

### Dev data locations

Build artifacts stay in `target/` (gitignored). The dev database defaults to `%LOCALAPPDATA%\SystemAtlas\dev\atlas.db`, deliberately outside the repo.

## Status

Phase 1 (MVP) in progress — see [docs/phases.md](docs/phases.md) for the live milestone tracker.
